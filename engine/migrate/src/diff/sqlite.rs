//! SQLite's schema as the diff engine's objects. SQLite keeps the `create` text each object was
//! written with (`sqlite_schema`) and records no dependency between objects, so every definition is
//! that text rendered again token by token, as the comparator renders it, every object is created
//! again from the target's own text, and the dependencies are read from the text: a view or a
//! trigger depends on the tables and views it names, and on the columns of those tables it names;
//! an index on its table and the columns it names; a table on the tables its foreign keys
//! reference (`pragma_foreign_key_list`). A table changes in place where SQLite's `alter table` can
//! (`add column`, `drop column`, `rename column`), and otherwise by the rebuild SQLite documents:
//! the new table made under another name, the rows copied, the old one dropped, the new one renamed,
//! and the indexes, triggers and views that went with the old one made again. Dropping a table where
//! foreign keys are on deletes its rows first, which fires the actions of the tables that reference
//! it, so those tables are rebuilt with it, at any depth, referencing the new one, each filled after
//! and dropped before what it references: the file keeps every row on whatever runs it, foreign keys
//! on or off. Only tables that reference each other have no such order, and defer their keys to the
//! commit, for the one transaction. An external-content full-text table (FTS4 or FTS5 with
//! `content='notes'`) holds an index of its content table's rows and none of its own: it is made
//! again with nothing lost, and each time it is made its index is rebuilt from those rows, as it is
//! at the end of the plan where a rebuild of its content table may give them other keys or values
//! than the index has.

use super::{Alterer, Change, Kind, Object, Schema, ROWS_ELSEWHERE};
use crate::db::{Db, Row};
use crate::sql::{tokenize, Token, TokenKind};
use crate::{Dialect, Error};
use std::collections::{BTreeMap, BTreeSet};

/// Separates the fields of an `extra` line, a character no SQL text holds.
const SEP: char = '\u{1f}';

/// Separates the items of one field of an `extra` line, another such character.
const LIST: char = '\u{1e}';

/// SQLite's keywords: a name that is one is quoted where a statement writes it.
const KEYWORDS: &[&str] = &[
    "abort", "action", "add", "after", "all", "alter", "always", "analyze", "and", "as", "asc", "attach", "autoincrement", "before", "begin", "between", "by", "cascade", "case", "cast", "check", "collate",
    "column", "commit", "conflict", "constraint", "create", "cross", "current", "current_date", "current_time", "current_timestamp", "database", "default", "deferrable", "deferred", "delete", "desc",
    "detach", "distinct", "do", "drop", "each", "else", "end", "escape", "except", "exclude", "exclusive", "exists", "explain", "fail", "filter", "first", "following", "for", "foreign", "from", "full",
    "generated", "glob", "group", "groups", "having", "if", "ignore", "immediate", "in", "index", "indexed", "initially", "inner", "insert", "instead", "intersect", "into", "is", "isnull", "join", "key",
    "last", "left", "like", "limit", "match", "materialized", "natural", "no", "not", "nothing", "notnull", "null", "nulls", "of", "offset", "on", "or", "order", "others", "outer", "over", "partition",
    "plan", "pragma", "preceding", "primary", "query", "raise", "range", "recursive", "references", "regexp", "reindex", "release", "rename", "replace", "restrict", "returning", "right", "rollback",
    "row", "rows", "savepoint", "select", "set", "table", "temp", "temporary", "then", "ties", "to", "transaction", "trigger", "unbounded", "union", "unique", "update", "using", "vacuum", "values",
    "view", "virtual", "when", "where", "window", "with", "without",
];

/// The words that open a table constraint rather than a column, as the comparator reads them.
const TABLE_CONSTRAINT: &[&str] = &["constraint", "primary", "unique", "check", "foreign"];

/// SQLite compares names without regard to ASCII case, and only ASCII case.
fn fold(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// A name as a statement writes it: bare where it can be, else in double quotes.
fn quote(name: &str) -> String {
    let bare = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if bare && !KEYWORDS.contains(&fold(name).as_str()) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A quoted name or a string without its quotes, anything else as written.
fn unquote(t: &Token) -> String {
    if !matches!(t.kind, TokenKind::Quoted | TokenKind::Str) || t.text.len() < 2 {
        return t.text.clone();
    }
    let inner = &t.text[1..t.text.len() - 1];
    match t.text.as_bytes()[0] {
        b'"' => inner.replace("\"\"", "\""),
        b'`' => inner.replace("``", "`"),
        b'\'' => inner.replace("''", "'"),
        _ => inner.to_string(),
    }
}

/// The name a word or a quoted name says, folded; None for any other token.
fn name_of(t: &Token) -> Option<String> {
    matches!(t.kind, TokenKind::Word | TokenKind::Quoted).then(|| fold(&unquote(t)))
}

/// The text rendered token by token as the comparator renders it: names unquoted and lowercased,
/// keywords lowercased, one space between tokens, literals exact.
fn canonical(tokens: &[Token]) -> String {
    let joined = tokens
        .iter()
        .map(|t| match t.kind {
            TokenKind::Quoted => {
                let inner = &t.text[1..t.text.len().saturating_sub(1).max(1)];
                inner.replace("\"\"", "\"").replace("``", "`").replace("]]", "]").to_lowercase()
            }
            TokenKind::Word => t.text.to_lowercase(),
            _ => t.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::with_capacity(joined.len());
    let chars: Vec<char> = joined.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' && matches!(chars.get(i + 1), Some(',' | ')')) {
            out.push(chars[i + 1]);
            out.push(' ');
            i += if chars.get(i + 2) == Some(&' ') { 3 } else { 2 };
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out.replace("( ", "(").replace(" .", ".").replace(". ", ".").trim().to_string()
}

/// `sql` with the tokens `with` answers for replaced.
fn rewrite(sql: &str, tokens: &[Token], mut with: impl FnMut(usize, &Token) -> Option<String>) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut at = 0;
    for (k, t) in tokens.iter().enumerate() {
        if let Some(text) = with(k, t) {
            out.push_str(&sql[at..t.start]);
            out.push_str(&text);
            at = t.end;
        }
    }
    out.push_str(&sql[at..]);
    out
}

/// The stored text as a statement of the plan: SQLite writes `CREATE TABLE` (or `INDEX`, `VIEW`,
/// `TRIGGER`) itself before the name, uppercase whatever the text was written in, so those words
/// are lowercased where the rest of the text is.
fn written(sql: &str) -> String {
    let tokens = tokenize(sql, Dialect::Sqlite);
    let Some(name) = name_at(&tokens) else { return sql.to_string() };
    let words = tokens[name..].iter().filter(|t| t.kind == TokenKind::Word && t.text.chars().any(|c| c.is_ascii_alphabetic()));
    let (lower, upper) = words.fold((0, 0), |(l, u), t| if t.text.chars().any(|c| c.is_ascii_lowercase()) { (l + 1, u) } else { (l, u + 1) });
    if lower < upper {
        return sql.to_string();
    }
    rewrite(sql, &tokens, |k, t| (k < name).then(|| t.text.to_lowercase()))
}

/// Where the object's name is in its stored text: after `CREATE [UNIQUE | VIRTUAL] TABLE` (or
/// `INDEX`, `VIEW`, `TRIGGER`), which is all SQLite keeps before it.
fn name_at(tokens: &[Token]) -> Option<usize> {
    let k = tokens.iter().take(4).position(|t| matches!(t.upper().as_str(), "TABLE" | "INDEX" | "VIEW" | "TRIGGER"))?;
    (k + 1 < tokens.len()).then_some(k + 1)
}

/// One part of a table's column list: a column or a table constraint.
#[derive(Clone, Debug)]
struct Part {
    tokens: Vec<Token>,
    /// As written.
    raw: String,
    /// The column's name, folded, or None for a table constraint.
    column: Option<String>,
}

impl Part {
    fn canonical(&self) -> String {
        canonical(&self.tokens)
    }
    /// The column's definition without its name.
    fn definition(&self) -> String {
        canonical(&self.tokens[1..])
    }
    /// The words outside parentheses, uppercased.
    fn words(&self) -> Vec<String> {
        let mut depth = 0;
        let mut out = Vec::new();
        for t in &self.tokens {
            match t.text.as_str() {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ if depth == 0 && t.kind == TokenKind::Word => out.push(t.upper()),
                _ => {}
            }
        }
        out
    }
}

/// The parts of a table's `create` text, split at the commas of its column list's top level as the
/// comparator splits them.
fn parts(sql: &str) -> Vec<Part> {
    let tokens = tokenize(sql, Dialect::Sqlite);
    let open = tokens.iter().position(|t| t.text == "(").map_or(tokens.len(), |at| at + 1);
    let mut depth = 0;
    let mut part: Vec<Token> = Vec::new();
    let mut groups: Vec<Vec<Token>> = Vec::new();
    for t in &tokens[open..] {
        if t.text == "(" {
            depth += 1;
        }
        if t.text == ")" {
            if depth == 0 {
                break;
            }
            depth -= 1;
        }
        if t.text == "," && depth == 0 {
            groups.push(std::mem::take(&mut part));
            continue;
        }
        part.push(t.clone());
    }
    if !part.is_empty() {
        groups.push(part);
    }
    groups
        .into_iter()
        .map(|tokens| {
            let raw = sql[tokens[0].start..tokens[tokens.len() - 1].end].to_string();
            let first = &tokens[0];
            let constraint = first.kind == TokenKind::Word && TABLE_CONSTRAINT.contains(&fold(&first.text).as_str());
            let column = (!constraint).then(|| fold(&unquote(first)));
            Part { tokens, raw, column }
        })
        .collect()
}

/// The names a text mentions, folded.
fn mentions(tokens: &[Token]) -> BTreeSet<String> {
    tokens.iter().filter_map(name_of).collect()
}

/// A column's default, read from its words: None where it has none, else whether it is a constant
/// (`add column` takes no other) and whether it is null.
fn default_of(part: &Part) -> Option<(bool, bool)> {
    let mut depth = 0;
    for (k, t) in part.tokens.iter().enumerate() {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ if depth == 0 && t.upper() == "DEFAULT" => {
                let rest = &part.tokens[k + 1..];
                let value: &[Token] = match rest.first().map(|t| t.text.as_str()) {
                    Some("(") => {
                        let close = rest.iter().position(|t| t.text == ")").unwrap_or(rest.len());
                        &rest[1..close]
                    }
                    _ => {
                        let signed = rest.first().is_some_and(|t| matches!(t.text.as_str(), "+" | "-"));
                        let blob = rest.first().is_some_and(|t| t.upper() == "X") && rest.get(1).is_some_and(|t| t.kind == TokenKind::Str && t.start == rest[0].end);
                        &rest[..rest.len().min(if signed || blob { 2 } else { 1 })]
                    }
                };
                let constant = match value {
                    [t] => matches!(t.kind, TokenKind::Number | TokenKind::Str) || matches!(t.upper().as_str(), "NULL" | "TRUE" | "FALSE"),
                    [sign, n] => (matches!(sign.text.as_str(), "+" | "-") && n.kind == TokenKind::Number) || (sign.upper() == "X" && n.kind == TokenKind::Str),
                    _ => false,
                };
                return Some((constant, value.len() == 1 && value[0].upper() == "NULL"));
            }
            _ => {}
        }
    }
    None
}

/// What a table's alterer needs to know beyond its text, kept in the table's `extra`.
#[derive(Debug, Default)]
struct Info {
    /// The table's name as SQLite stores it.
    name: String,
    virtual_table: bool,
    autoincrement: bool,
    /// Whether its rows have a rowid no column of it names (no INTEGER PRIMARY KEY, not WITHOUT ROWID).
    bare_rowid: bool,
    /// The column that is its rowid (its INTEGER PRIMARY KEY), folded.
    alias: Option<String>,
    /// An external-content full-text table's content table, the column of it its index is keyed
    /// on (or `rowid`), and the columns it indexes, each folded.
    content: Option<(String, String, Vec<String>)>,
    /// Each column's flags: `a` added in place (`add column` takes it on a table with rows), `d`
    /// dropped in place, `g` generated (no insert writes it), `n` not null with nothing to fill it.
    flags: BTreeMap<String, String>,
    /// What its rebuild takes down and makes again, in the order it was made: id, what decides
    /// whether the planner replaces it itself, its drop (none where it goes with what it is on),
    /// its create, and where it stands among the schema's objects.
    goes: Vec<(String, String, String, String, usize)>,
    /// The names of its schema a rebuild's own tables and indexes could take.
    taken: BTreeSet<String>,
}

impl Info {
    fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("name{SEP}{}", self.name)];
        for (flag, on) in [("virtual", self.virtual_table), ("autoincrement", self.autoincrement), ("rowid", self.bare_rowid)] {
            if on {
                out.push(flag.to_string());
            }
        }
        out.extend(self.alias.iter().map(|c| format!("alias{SEP}{c}")));
        if let Some((table, key, read)) = &self.content {
            out.push(format!("content{SEP}{table}{SEP}{key}{SEP}{}", read.join(&LIST.to_string())));
            out.push(ROWS_ELSEWHERE.to_string());
        }
        out.extend(self.flags.iter().map(|(c, f)| format!("column{SEP}{c}{SEP}{f}")));
        out.extend(self.goes.iter().map(|(id, fingerprint, drop, create, rank)| format!("goes{SEP}{id}{SEP}{fingerprint}{SEP}{drop}{SEP}{create}{SEP}{rank}")));
        out.extend(self.taken.iter().map(|t| format!("taken{SEP}{t}")));
        out
    }

    fn of(o: &Object) -> Info {
        let mut info = Info::default();
        for line in &o.extra {
            let f: Vec<&str> = line.split(SEP).collect();
            match f[..] {
                ["name", name] => info.name = name.to_string(),
                ["virtual"] => info.virtual_table = true,
                ["autoincrement"] => info.autoincrement = true,
                ["rowid"] => info.bare_rowid = true,
                ["alias", c] => info.alias = Some(c.to_string()),
                ["content", table, key, read] => info.content = Some((table.to_string(), key.to_string(), read.split(LIST).filter(|c| !c.is_empty()).map(String::from).collect())),
                ["column", c, flags] => {
                    info.flags.insert(c.to_string(), flags.to_string());
                }
                ["goes", id, fingerprint, drop, create, rank] => info.goes.push((id.to_string(), fingerprint.to_string(), drop.to_string(), create.to_string(), rank.parse().unwrap_or(0))),
                ["taken", t] => {
                    info.taken.insert(t.to_string());
                }
                _ => {}
            }
        }
        info
    }

    fn flag(&self, column: &str, flag: char) -> bool {
        self.flags.get(column).is_some_and(|f| f.contains(flag))
    }
}

/// The schema of the database `db` is connected to.
pub async fn read(db: &mut Db) -> Result<Schema, Error> {
    let list: BTreeMap<String, Row> = db.rows("select name, type, strict, wr from pragma_table_list where schema = 'main'").await?.into_iter().map(|r| (fold(r.text("name")), r)).collect();
    let shadow = |name: &str| list.get(&fold(name)).is_some_and(|r| r.text("type") == "shadow");
    let all: Vec<Row> = db
        .rows(&format!("select type, name, tbl_name as tbl, sql from sqlite_schema where {} and sql is not null order by rowid", crate::catalog::sqlite_project("tbl_name")))
        .await?;
    // SQLite takes a table named as a full-text table's own (`docs_content`) for one, whoever made
    // it: one that an external-content index reads its text from is the project's, and so is one
    // beside a full-text table whose text is elsewhere, which keeps no `_content` of its own.
    let mut contents: BTreeSet<String> = BTreeSet::new();
    for r in all.iter().filter(|r| r.text("type") == "table") {
        let tokens = tokenize(r.text("sql"), Dialect::Sqlite);
        if fts_args(&tokens).is_some_and(|(_, args)| args.iter().any(|a| option(a).is_some_and(|(k, _)| k == "content"))) {
            contents.insert(format!("{}_content", fold(r.text("name"))));
        }
        contents.extend(external_content(&tokens).map(|(table, ..)| table));
    }
    let rows: Vec<Row> = all.into_iter().filter(|r| contents.contains(&fold(r.text("tbl"))) || !shadow(r.text("tbl"))).collect();

    // Every table's and view's id by its folded name, and each table's columns.
    let mut relations: BTreeMap<String, String> = BTreeMap::new();
    let mut columns: BTreeMap<String, Vec<Part>> = BTreeMap::new();
    for r in &rows {
        let name = fold(r.text("name"));
        match r.text("type") {
            "table" => {
                relations.insert(name.clone(), format!("table {name}"));
                if !r.text("sql").to_ascii_uppercase().starts_with("CREATE VIRTUAL") {
                    columns.insert(name, parts(r.text("sql")).into_iter().filter(|p| p.column.is_some()).collect());
                }
            }
            "view" => {
                relations.insert(name.clone(), format!("view {name}"));
            }
            _ => {}
        }
    }
    let names: BTreeSet<String> = rows.iter().map(|r| fold(r.text("name"))).collect();
    // What a text depends on: the tables and views it names, and those tables' columns it names.
    let deps_of = |tokens: &[Token], own: &str| -> BTreeSet<String> {
        let named = mentions(tokens);
        let mut out = BTreeSet::new();
        for n in &named {
            let Some(id) = relations.get(n) else { continue };
            if id == own {
                continue;
            }
            out.insert(id.clone());
            for c in columns.get(n).into_iter().flatten() {
                let c = c.column.as_deref().unwrap_or_default();
                if named.contains(c) {
                    out.insert(format!("column {n}.{c}"));
                }
            }
        }
        out
    };

    let mut s = Schema::default();
    let mut infos: BTreeMap<String, Info> = BTreeMap::new();
    for r in &rows {
        let (name, sql) = (r.text("name"), r.text("sql"));
        let folded = fold(name);
        let tokens = tokenize(sql, Dialect::Sqlite);
        let body = name_at(&tokens).map_or(&tokens[..], |at| &tokens[at + 1..]);
        match r.text("type") {
            "table" if sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL") => {
                let id = format!("table {folded}");
                let content = external_content(&tokens);
                let mut create = vec![written(sql)];
                let mut deps = BTreeSet::new();
                // An external-content index is made after its content table, as the target has
                // it, and filled from its rows: made over a table that has rows, or made again,
                // its index would otherwise be empty, and every search would miss them.
                if let Some((table, read_columns, key)) = &content {
                    if let Some(relation) = relations.get(table).filter(|r| **r != id) {
                        deps.insert(relation.clone());
                        for c in read_columns.iter().chain([key]) {
                            if columns.get(table).is_some_and(|ps| ps.iter().any(|p| p.column.as_deref() == Some(c.as_str()))) {
                                deps.insert(format!("column {table}.{c}"));
                            }
                        }
                        // The rebuild reads each column it indexes, and its key, from the content
                        // table or view, and fails on one that lacks any: an index that can't be
                        // filled is made as it was written.
                        let has: BTreeSet<String> = db.rows(&format!("select name from pragma_table_xinfo({})", literal(table))).await?.iter().map(|r| fold(r.text("name"))).collect();
                        let rowid = relation.starts_with("table ") && list.get(table).is_some_and(|o| o.text("wr") != "1");
                        let keyed = has.contains(key) || (matches!(key.as_str(), "rowid" | "oid" | "_rowid_") && rowid);
                        if keyed && read_columns.iter().all(|c| has.contains(c)) {
                            create.push(rebuild(name));
                        }
                    }
                }
                infos.insert(id.clone(), Info { name: name.to_string(), virtual_table: true, content: content.map(|(table, read, key)| (table, key, read)), ..Info::default() });
                s.add(Object { id, kind: Some(Kind::Table), def: format!("virtual {}", canonical(&tokens)), create, drop: vec![format!("drop table {}", quote(name))], deps, ..Object::default() });
            }
            "table" => {
                let id = format!("table {folded}");
                let table_parts = parts(sql);
                let option = list.get(&folded);
                let options = [option.is_some_and(|o| o.text("strict") == "1").then_some("strict"), option.is_some_and(|o| o.text("wr") == "1").then_some("without rowid")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                let lines: BTreeSet<String> = table_parts.iter().map(Part::canonical).collect();
                let def = format!("table {options}\n{}", lines.into_iter().collect::<Vec<_>>().join("\n"));
                let xinfo = db.rows(&format!("select name, type, pk, hidden from pragma_table_xinfo({})", literal(name))).await?;
                let hidden: BTreeMap<String, String> = xinfo.iter().map(|c| (fold(c.text("name")), c.text("hidden").to_string())).collect();
                let keys: Vec<&Row> = xinfo.iter().filter(|c| c.text("pk") != "0").collect();
                let wr = option.is_some_and(|o| o.text("wr") == "1");
                // A lone INTEGER PRIMARY KEY is the rowid itself, but where the column says
                // `primary key desc`, which SQLite keeps as an ordinary column.
                let descending = |key: &Row| table_parts.iter().any(|p| p.column.as_deref() == Some(fold(key.text("name")).as_str()) && p.words().windows(3).any(|w| w == ["PRIMARY", "KEY", "DESC"]));
                let alias = !wr && keys.len() == 1 && keys[0].text("type").eq_ignore_ascii_case("integer") && !descending(keys[0]);
                let mut info = Info {
                    name: name.to_string(),
                    autoincrement: tokens.iter().any(|t| t.upper() == "AUTOINCREMENT"),
                    bare_rowid: !wr && !alias,
                    alias: alias.then(|| fold(keys[0].text("name"))),
                    taken: names.iter().filter(|n| n.starts_with("new_") || n.starts_with("reylo_aside_")).cloned().collect(),
                    ..Info::default()
                };
                for (k, p) in table_parts.iter().enumerate() {
                    let Some(c) = &p.column else { continue };
                    let words = p.words();
                    let has = |w: &str| words.iter().any(|x| x == w);
                    let generated = matches!(hidden.get(c).map(String::as_str), Some("2" | "3"));
                    let stored = hidden.get(c).map(String::as_str) == Some("3");
                    let not_null = words.windows(2).any(|w| w[0] == "NOT" && w[1] == "NULL");
                    let default = default_of(p);
                    let filled = generated || default.is_some_and(|(_, null)| !null);
                    let addable = !has("PRIMARY") && !has("UNIQUE") && !stored && default.is_none_or(|(constant, _)| constant) && (!not_null || filled) && !(has("REFERENCES") && default.is_some_and(|(_, null)| !null));
                    // Another part that names it (a check, a key, a generated column) keeps it.
                    let named = table_parts.iter().enumerate().any(|(j, q)| j != k && mentions(&q.tokens).contains(c));
                    let droppable = !has("PRIMARY") && !has("UNIQUE") && !has("REFERENCES") && !named;
                    let mut flags = String::new();
                    for (flag, on) in [('a', addable), ('d', droppable), ('g', generated), ('n', not_null && !filled && !(alias && has("PRIMARY")))] {
                        if on {
                            flags.push(flag);
                        }
                    }
                    info.flags.insert(c.clone(), flags);
                    s.add(Object {
                        id: format!("column {folded}.{c}"),
                        kind: Some(Kind::Column),
                        owner: Some(id.clone()),
                        def: p.definition(),
                        inline: true,
                        change: Change::Alter { blocks: false },
                        extra: vec![quote(name), p.tokens[0].text.clone()],
                        ..Object::default()
                    });
                }
                let mut deps = BTreeSet::new();
                for f in db.rows(&format!("select \"table\" as t from pragma_foreign_key_list({})", literal(name))).await? {
                    if let Some(target) = relations.get(&fold(f.text("t"))).filter(|t| **t != id) {
                        deps.insert(target.clone());
                    }
                }
                infos.insert(id.clone(), info);
                s.add(Object { id, kind: Some(Kind::Table), def, create: vec![written(sql)], drop: vec![format!("drop table {}", quote(name))], deps, change: Change::Alter { blocks: false }, ..Object::default() });
            }
            kind @ ("index" | "view" | "trigger") => {
                let id = format!("{kind} {folded}");
                let on = fold(r.text("tbl"));
                let owner = match kind {
                    "view" => None,
                    _ => relations.get(&on).cloned(),
                };
                let mut deps = deps_of(body, &id);
                if let Some(owner) = &owner {
                    deps.remove(owner);
                }
                s.add(Object {
                    id,
                    kind: Some(match kind {
                        "index" => Kind::Index,
                        "view" => Kind::View,
                        _ => Kind::Trigger,
                    }),
                    owner,
                    def: canonical(&tokens),
                    create: vec![written(sql)],
                    drop: vec![format!("drop {kind} {}", quote(name))],
                    deps,
                    ..Object::default()
                });
            }
            _ => {}
        }
    }

    // What a rebuild of each table takes down and makes again. Its drop takes its own indexes and
    // triggers with it; and SQLite refuses to rename the new table into place while any view or
    // trigger names a table or view that is not there, so every view and trigger that names it, or
    // names a view taken down with it, goes too. The planner replaces some of these itself (one that
    // changes, and one that reaches a view or a virtual table that changes), so each carries its
    // definition and those of what it reaches, and the rebuild leaves the ones that differ to it.
    let reach = |id: &str| -> String {
        let o = &s.objects[id];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<String> = o.deps.iter().chain(o.owner.iter()).cloned().collect();
        while let Some(next) = queue.pop() {
            let Some(o) = s.objects.get(&next) else { continue };
            let replaced = o.kind == Some(Kind::View) || (o.change == Change::Replace && o.kind == Some(Kind::Table));
            if replaced && seen.insert(format!("{next}={}", o.def)) {
                queue.extend(o.deps.iter().chain(o.owner.iter()).cloned());
            }
        }
        format!("{}\n{}", o.def, seen.into_iter().collect::<Vec<_>>().join("\n"))
    };
    let ordered: Vec<String> = rows.iter().map(|r| format!("{} {}", r.text("type"), fold(r.text("name")))).filter(|id| s.objects.get(id).is_some_and(|o| matches!(o.kind, Some(Kind::Index | Kind::View | Kind::Trigger)))).collect();
    for (table, info) in infos.iter_mut() {
        if info.virtual_table {
            continue;
        }
        // The views it reaches, and what goes with it, each with whether it needs a drop of its own.
        let mut down: BTreeSet<String> = BTreeSet::from([table.clone()]);
        let mut goes: BTreeMap<String, bool> = BTreeMap::new();
        loop {
            let before = goes.len();
            for id in &ordered {
                let o = &s.objects[id];
                let with_owner = o.owner.as_ref().is_some_and(|owner| down.contains(owner));
                if with_owner || (matches!(o.kind, Some(Kind::View | Kind::Trigger)) && o.deps.iter().any(|d| down.contains(d))) {
                    goes.entry(id.clone()).or_insert(!with_owner);
                    if o.kind == Some(Kind::View) {
                        down.insert(id.clone());
                    }
                }
            }
            if goes.len() == before {
                break;
            }
        }
        for (rank, id) in ordered.iter().enumerate().filter(|(_, id)| goes.contains_key(*id)) {
            let o = &s.objects[id];
            let drop = if goes[id] { o.drop[0].clone() } else { String::new() };
            info.goes.push((id.clone(), reach(id), drop, o.create[0].clone(), rank));
        }
    }
    for (id, info) in infos {
        if let Some(o) = s.objects.get_mut(&id) {
            o.extra = info.lines();
        }
    }
    // What the file keeps of the project's own, where it is set: `pragma user_version` and
    // `pragma application_id`. One the target no longer sets is left as it is, as an app may set
    // its own.
    for pragma in ["user_version", "application_id"] {
        let value = db.rows(&format!("select {pragma} as v from pragma_{pragma}")).await?.first().map(|r| r.text("v").to_string()).unwrap_or_default();
        if !value.is_empty() && value != "0" {
            s.add(Object { id: format!("pragma {pragma}"), kind: Some(Kind::ColumnSetting), def: value.clone(), create: vec![format!("pragma {pragma} = {value}")], ..Object::default() });
        }
    }
    Ok(s)
}

#[derive(Default)]
pub struct SqliteAlterer {
    decided: super::Decided,
    schemas: std::cell::RefCell<Option<(Schema, Schema)>>,
    /// The tables each table's change makes again, by its id, as the plan's ordering asks.
    remade: std::cell::RefCell<BTreeMap<String, BTreeSet<String>>>,
}

/// The affinity of a SQLite column's declared type, by SQLite's rules (datatype3.html, 3.1), as far as
/// a change between two affinities can lose values.
fn affinity(def: &str) -> super::TypeClass {
    use super::TypeClass;
    const ENDS: [&str; 11] = ["CONSTRAINT", "PRIMARY", "NOT", "NULL", "UNIQUE", "CHECK", "DEFAULT", "COLLATE", "REFERENCES", "GENERATED", "AS"];
    let declared: Vec<&str> = def.split_whitespace().take_while(|w| !ENDS.contains(&w.to_ascii_uppercase().as_str())).collect();
    let t = declared.join(" ").to_ascii_uppercase();
    if t.contains("INT") {
        TypeClass::Int(64)
    } else if t.contains("CHAR") || t.contains("CLOB") || t.contains("TEXT") || t.is_empty() || t.contains("BLOB") {
        TypeClass::Text
    } else if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
        TypeClass::Float(15)
    } else {
        TypeClass::Numeric(None)
    }
}

impl SqliteAlterer {
    /// Whether the change of table `by` makes table `id` again, as a rebuild makes those that reference it.
    fn makes_again(&self, by: &str, id: &str) -> bool {
        if let Some(made) = self.remade.borrow().get(by) {
            return made.contains(id);
        }
        let schemas = self.schemas.borrow();
        let Some(both) = schemas.as_ref() else { return false };
        let (Some(from), Some(to)) = (both.0.objects.get(by), both.1.objects.get(by)) else { return false };
        let out = alter_table(from, to, &|name| self.decided.of(name), Some(both)).unwrap_or_default();
        let dropped = |o: &&Object| out.iter().any(|s| *s == format!("drop table {}", quote(&Info::of(o).name)));
        let made: BTreeSet<String> = both.0.objects.values().filter(|o| o.kind == Some(Kind::Table)).filter(dropped).map(|o| o.id.clone()).collect();
        let answer = made.contains(id);
        self.remade.borrow_mut().insert(by.to_string(), made);
        answer
    }
}

impl Alterer for SqliteAlterer {
    fn narrows(&self, from: &Object, to: &Object) -> Option<String> {
        if from.kind != Some(Kind::Column) {
            return None;
        }
        // Only text turns into numbers: an INTEGER column keeps 1.5 as it is, and so on.
        let from_affinity = affinity(&from.def);
        if from_affinity != super::TypeClass::Text {
            return None;
        }
        let why = super::narrowing(&from_affinity, &affinity(&to.def))?;
        let declared = |def: &str| def.split_whitespace().next().unwrap_or("none").to_lowercase();
        Some(format!("changes from {} to {}, which {why}", declared(&from.def), declared(&to.def)))
    }

    fn schemas(&self, from: &Schema, to: &Schema) {
        *self.schemas.borrow_mut() = Some((from.clone(), to.clone()));
        self.remade.borrow_mut().clear();
    }

    /// Every full-text index's rebuild runs once, after every other statement: where the plan's
    /// renames have given its key the name the target reads it by, and every view it reads
    /// through is back. Each comes after those of the indexes its content reads (a view that
    /// searches another index), and an index a table's rebuild refreshes that the plan makes again
    /// is rebuilt with the rest, as it is made.
    fn finish(&self, statements: &mut Vec<String>) {
        let mut rebuilds: Vec<(String, String)> = Vec::new();
        statements.retain(|s| match rebuilt(s) {
            Some(name) => {
                if !rebuilds.iter().any(|(n, _)| *n == name) {
                    rebuilds.push((name, s.clone()));
                }
                false
            }
            None => true,
        });
        // The indexes each one's content reads, by the target's schema: its content where that
        // is another index, the index an `fts5vocab` table reads, and what a view on the way
        // reads, which `view_reads` gives: every name its text holds (`loose`, which never misses
        // a read), or the sources of its FROM clauses (which no alias or literal fakes).
        let schemas = self.schemas.borrow();
        let names: Vec<String> = rebuilds.iter().map(|(n, _)| n.clone()).collect();
        let reads = |name: &str, loose: bool| -> BTreeSet<String> {
            let mut out = BTreeSet::new();
            let Some((_, to)) = schemas.as_ref() else { return out };
            let Some(content) = to.objects.get(&format!("table {name}")).and_then(|o| Info::of(o).content).map(|(c, ..)| c) else { return out };
            let (mut queue, mut seen) = (vec![content], BTreeSet::new());
            while let Some(relation) = queue.pop() {
                if !seen.insert(relation.clone()) {
                    continue;
                }
                if names.contains(&relation) {
                    out.insert(relation.clone());
                }
                // An index the content is, which this rebuild doesn't refresh, reads its own.
                if let Some(o) = to.objects.get(&format!("table {relation}")) {
                    queue.extend(Info::of(o).content.map(|(c, ..)| c));
                    queue.extend(o.create.first().and_then(|sql| vocabulary_of(sql)));
                }
                if let Some(view) = to.objects.get(&format!("view {relation}")) {
                    queue.extend(view_reads(view.create.first().map_or("", String::as_str), loose));
                }
            }
            out.remove(name);
            out
        };
        let loose: Vec<BTreeSet<String>> = names.iter().map(|n| reads(n, true)).collect();
        let tight: Vec<BTreeSet<String>> = names.iter().map(|n| reads(n, false)).collect();
        // Each after every index it may read, in the plan's order otherwise; where those make a
        // circle, the next is one whose FROM clauses read none still to come.
        let mut done: Vec<String> = Vec::new();
        let mut left: Vec<usize> = (0..names.len()).collect();
        while !left.is_empty() {
            let ready = |needs: &[BTreeSet<String>]| left.iter().position(|&k| needs[k].iter().all(|n| done.contains(n)));
            let at = ready(&loose).or_else(|| ready(&tight)).unwrap_or(0);
            let k = left.remove(at);
            statements.push(rebuilds[k].1.clone());
            done.push(names[k].clone());
        }
    }

    fn decide(&self, renames: &[(String, String)]) {
        self.decided.set(renames);
    }

    /// A table's rebuild makes the tables that reference it, at any depth, again from the text each
    /// has then: so each one's own change waits for every rebuild that makes it again, rather than
    /// run first and be undone. A table it references whose change runs in place makes nothing
    /// again, and waiting for it could close a circle where none was. A rebuild that takes others
    /// of tables that reference each other to the target waits for what each of those needs there
    /// too: a table one comes to reference by a key it has yet to have, which a copied row looks
    /// up with foreign keys on.
    fn waits_for(&self, from: &Object, to: &Object) -> Vec<String> {
        if to.kind != Some(Kind::Table) {
            return Vec::new();
        }
        let schemas = self.schemas.borrow();
        let Some((f, t)) = schemas.as_ref() else { return Vec::new() };
        let mut candidates = parents_of(f, t, &from.id);
        candidates.extend(circle(f, t, &from.id).into_iter().filter(|m| *m != from.id && !candidates.contains(m)).collect::<Vec<_>>());
        let mut out: Vec<String> = candidates.into_iter().filter(|p| self.makes_again(p, &from.id)).collect();
        let decided = |name: &str| self.decided.of(name);
        if rebuilt_by(f, t, &from.id, &decided).as_deref() == Some(from.id.as_str()) {
            let circling = circle(f, t, &from.id);
            for m in circling.iter().filter(|m| **m != from.id && self.makes_again(&from.id, m)) {
                let Some(target) = t.objects.get(m) else { continue };
                let carried = f.objects.get(m).is_some_and(|x| m.strip_prefix("table ").and_then(|name| shape(x, target, decided(name))).is_some_and(|s| s.rebuild));
                let text = target.create.first().map_or("", String::as_str);
                let wanted = |d: &str| t.objects.get(d).is_some_and(|o| o.kind == Some(Kind::Table) && new_key(text, f, o));
                if carried {
                    out.extend(target.deps.iter().filter(|d| !circling.contains(*d) && !out.contains(*d) && wanted(d)).cloned().collect::<Vec<_>>());
                }
            }
        }
        out
    }

    /// A table's change, or a table made, needs the change of a table it references first only
    /// where that is a rebuild: one in place moves no key a copied row looks up, and makes nothing
    /// again. Nor does a rebuild of its own that makes that table again, whose change then follows
    /// it, nor one beside it of the tables that reference each other, which one rebuild takes to the
    /// target together.
    fn runs_before(&self, to: &Object, dep_from: &Object, dep_to: &Object) -> bool {
        if to.kind != Some(Kind::Table) || dep_to.kind != Some(Kind::Table) {
            return false;
        }
        let schemas = self.schemas.borrow();
        let Some((f, t)) = schemas.as_ref() else { return false };
        let decided = |name: &str| self.decided.of(name);
        let rebuilds = |a: &Object, b: &Object| b.id.strip_prefix("table ").and_then(|name| shape(a, b, decided(name))).map(|s| s.rebuild);
        match rebuilds(dep_from, dep_to) {
            Some(false) => true,
            Some(true) => {
                let Some(from) = f.objects.get(&to.id) else { return false };
                self.makes_again(&to.id, &dep_to.id) || (rebuilds(from, to) == Some(true) && circle(f, t, &to.id).contains(&dep_to.id))
            }
            None => false,
        }
    }

    fn rename(&self, from: &Object, to: &Object) -> Option<Vec<String>> {
        let (table, old, new) = (from.extra.first()?, from.extra.get(1)?, to.extra.get(1)?);
        Some(vec![format!("alter table {table} rename column {old} to {new}")])
    }

    fn alter(&self, from: &Object, to: &Object) -> Option<Vec<String>> {
        match to.kind? {
            // A column changes with its table, whose alter sees the whole of both.
            Kind::Column => Some(vec![]),
            Kind::Table => alter_table(from, to, &|name| self.decided.of(name), self.schemas.borrow().as_ref()),
            _ => None,
        }
    }
}

/// A change of table `from` to `to` as SQLite takes it: the target's text with each column the plan
/// renames still under its old name, as the statements write it, and whether `alter table` can take
/// the table there, dropping and adding columns, or it is rebuilt.
struct Shape {
    a: Info,
    b: Info,
    old_parts: Vec<Part>,
    new_parts: Vec<Part>,
    old_columns: Vec<(String, String)>,
    new_columns: Vec<(String, String)>,
    /// Each column renamed, by its new name, to its old.
    renamed: BTreeMap<String, String>,
    target: String,
    /// The target's flags, each column by its name before the renames.
    new_flags: BTreeMap<String, String>,
    /// Whether its options (WITHOUT ROWID, STRICT) change.
    reshaped: bool,
    rebuild: bool,
    drops: Vec<String>,
    adds: Vec<String>,
}

impl Shape {
    fn flag(&self, c: &str, f: char) -> bool {
        self.new_flags.get(c).is_some_and(|x| x.contains(f))
    }

    fn has(ps: &[Part], c: &str) -> bool {
        ps.iter().any(|p| p.column.as_deref() == Some(c))
    }

    /// The columns its rows copy into a rebuild: those both sides have, but those the target generates.
    fn copied(&self) -> Vec<String> {
        self.new_parts.iter().filter(|p| p.column.as_deref().is_some_and(|c| Self::has(&self.old_parts, c) && !self.flag(c, 'g'))).map(|p| p.tokens[0].text.clone()).collect()
    }

    /// Every column either side names.
    fn named(&self) -> Vec<String> {
        self.old_columns.iter().chain(self.new_columns.iter()).map(|(c, _)| c.clone()).collect()
    }

    /// The columns the change drops, which the planner replaces what is on them for.
    fn dropped(&self) -> BTreeSet<String> {
        let (old, new) = (&self.old_columns, &self.new_columns);
        old.iter().map(|(c, _)| c).filter(|c| !new.iter().any(|(n, _)| n == *c) && !self.renamed.values().any(|o| o == *c)).map(|c| format!("column {}.{c}", fold(&self.a.name))).collect()
    }

    /// What a rebuild says of each column it adds that no row can fill.
    fn warnings(&self) -> Vec<String> {
        let new = self.new_parts.iter().filter(|p| p.column.as_deref().is_some_and(|c| !Self::has(&self.old_parts, c) && self.flag(c, 'n')));
        new.map(|p| format!("-- {} is not null with no default, so the copy fails on a table that has rows: give it a default, or add it nullable, fill it, then make it not null", p.tokens[0].text)).collect()
    }

    /// The full-text indexes the change leaves stale.
    fn reindexed(&self, schemas: Option<&(Schema, Schema)>) -> Vec<String> {
        reindexed(&self.a, &self.b, &self.renamed, &self.old_parts, &self.new_parts, &self.named(), self.reshaped, schemas)
    }
}

/// What a change of table `from` to `to` is (`Shape`), the columns the planner renamed in it, by old
/// name and new, `decided`.
fn shape(from: &Object, to: &Object, decided: Option<Vec<(String, String)>>) -> Option<Shape> {
    let (a, b) = (Info::of(from), Info::of(to));
    if a.virtual_table || b.virtual_table {
        return None;
    }
    let (old_sql, new_sql) = (from.create.first()?, to.create.first()?);
    let old_parts = parts(old_sql);
    let columns = |ps: &[Part]| -> Vec<(String, String)> { ps.iter().filter_map(|p| Some((p.column.clone()?, p.definition()))).collect() };
    let (old_columns, new_columns) = (columns(&old_parts), columns(&parts(new_sql)));

    // The renames, found as the planner finds them: a column that goes while one of its definition
    // comes, alone in its table.
    let gone: Vec<&(String, String)> = old_columns.iter().filter(|(c, _)| !new_columns.iter().any(|(n, _)| n == c)).collect();
    let come: Vec<&(String, String)> = new_columns.iter().filter(|(c, _)| !old_columns.iter().any(|(n, _)| n == c)).collect();
    let mut renamed: BTreeMap<String, String> = BTreeMap::new();
    match decided {
        // The planner's decision, which may rename what its rule alone would not.
        Some(pairs) => renamed.extend(pairs.into_iter().map(|(old, new)| (new, old))),
        None => {
            for (old, def) in &gone {
                let same: Vec<&String> = come.iter().filter(|(_, d)| d == def).map(|(c, _)| c).collect();
                if same.len() == 1 && gone.iter().filter(|(_, d)| d == def).count() == 1 {
                    renamed.insert(same[0].clone(), old.clone());
                }
            }
        }
    }
    let written_as: BTreeMap<String, String> = old_parts.iter().filter_map(|p| Some((p.column.clone()?, p.tokens[0].text.clone()))).collect();
    let target = before_renames(new_sql, &renamed, &written_as);
    let new_parts = parts(&target);
    let new_flags: BTreeMap<String, String> = b.flags.iter().map(|(c, f)| (renamed.get(c).cloned().unwrap_or_else(|| c.clone()), f.clone())).collect();
    let flag = |c: &str, f: char| new_flags.get(c).is_some_and(|x| x.contains(f));

    let constraints = |ps: &[Part]| -> BTreeSet<String> { ps.iter().filter(|p| p.column.is_none()).map(Part::canonical).collect() };
    let options = |def: &str| def.lines().next().unwrap_or_default().to_string();
    let find = |ps: &'_ [Part], c: &str| ps.iter().find(|p| p.column.as_deref() == Some(c)).cloned();
    let reshaped = options(&from.def) != options(&to.def);
    let mut rebuild = reshaped || constraints(&old_parts) != constraints(&new_parts);
    let mut drops = Vec::new();
    let mut adds = Vec::new();
    for p in old_parts.iter().filter(|p| p.column.is_some()) {
        let c = p.column.as_deref().unwrap_or_default();
        match find(&new_parts, c) {
            Some(q) if q.canonical() != p.canonical() => rebuild = true,
            Some(_) => {}
            None if a.flag(c, 'd') => drops.push(p.tokens[0].text.clone()),
            None => rebuild = true,
        }
    }
    for p in new_parts.iter().filter(|p| p.column.is_some()) {
        let c = p.column.as_deref().unwrap_or_default();
        if find(&old_parts, c).is_none() {
            if flag(c, 'a') {
                adds.push(p.raw.clone());
            } else {
                rebuild = true;
            }
        }
    }
    Some(Shape { a, b, old_parts, new_parts, old_columns, new_columns, renamed, target, new_flags, reshaped, rebuild, drops, adds })
}

/// The columns the planner renamed in a table, by old name and new, from the table's name as its id writes it.
type Renamed<'a> = &'a dyn Fn(&str) -> Option<Vec<(String, String)>>;

/// The statements that take table `from` to `to`: `alter table` where SQLite can, else a rebuild.
/// A column the planner renames is renamed after this, so the statements write it by its old name.
fn alter_table(from: &Object, to: &Object, decided: Renamed, schemas: Option<&(Schema, Schema)>) -> Option<Vec<String>> {
    let s = shape(from, to, decided(from.id.strip_prefix("table ")?))?;
    let (a, b, target) = (&s.a, &s.b, &s.target);
    let table = quote(&a.name);
    if !s.rebuild {
        let mut out: Vec<String> = s.drops.iter().map(|c| format!("alter table {table} drop column {c}")).collect();
        out.extend(s.adds.iter().map(|p| format!("alter table {table} add column {p}")));
        return Some(out);
    }
    // Of tables that reference each other, the first by name whose change is a rebuild makes the
    // others as the target has them, so that no rebuild makes one again as it was after its own.
    if schemas.and_then(|(f, t)| rebuilt_by(f, t, &from.id, decided)).is_some_and(|by| by != from.id) {
        return Some(Vec::new());
    }

    // The tables made again with it: those that reference it back as the target has them, and those
    // that reference any of these, at any depth, as they are on both sides.
    let circling = schemas.map(|(f, t)| circle(f, t, &from.id)).unwrap_or_default();
    let mut children: Vec<(&Object, &Object)> = Vec::new();
    if let Some((f, t)) = schemas {
        let members = circling.iter().filter(|m| **m != from.id).filter_map(|m| f.objects.get(m).zip(t.objects.get(m)));
        let roots = std::iter::once(&from.id).chain(circling.iter().filter(|m| **m != from.id));
        for (x, y) in roots.flat_map(|m| children_of(f, t, m)).chain(members) {
            if x.id != from.id && !children.iter().any(|(c, _)| c.id == x.id) {
                children.push((x, y));
            }
        }
    }
    // Each of them as it is made again: its text, the columns its rows copy and every column it names. One it
    // references back whose own change is a rebuild is made as the target has it, and the rest as they are.
    let mut carried: Vec<(String, Vec<String>, Vec<String>, Option<Shape>)> = Vec::new();
    for (x, y) in &children {
        let own = if circling.contains(&x.id) { x.id.strip_prefix("table ").and_then(|name| shape(x, y, decided(name))).filter(|s| s.rebuild) } else { None };
        carried.push(match own {
            Some(own) => (own.target.clone(), own.copied(), own.named(), Some(own)),
            None => {
                let (text, info) = (x.create.first()?.clone(), Info::of(x));
                let ps = parts(&text);
                let copied = ps.iter().filter(|p| p.column.as_deref().is_some_and(|c| !info.flag(c, 'g'))).map(|p| p.tokens[0].text.clone()).collect();
                let named = ps.iter().filter_map(|p| p.column.clone()).collect();
                (text, copied, named, None)
            }
        });
    }
    // Filled in the order their texts reference each other, and dropped in the reverse. The rebuilt
    // table comes first: where it references one of the others, or the others reference each other
    // in a circle, no order fills them.
    let names: Vec<String> = children.iter().map(|(x, _)| fold(&Info::of(x).name)).collect();
    let references = |text: &str| -> BTreeSet<String> {
        let tokens = tokenize(text, Dialect::Sqlite);
        (0..tokens.len()).filter(|&k| tokens[k].upper() == "REFERENCES").filter_map(|k| tokens.get(k + 1).and_then(qualifier)).collect()
    };
    let mut cycle = references(target).iter().any(|r| names.contains(r));
    let (mut order, mut left): (Vec<usize>, Vec<usize>) = (Vec::new(), (0..children.len()).collect());
    while !left.is_empty() {
        let ready = |k: usize| references(&carried[k].0).iter().all(|r| *r == names[k] || !names.contains(r) || order.iter().any(|&o| names[o] == *r));
        let at = left.iter().position(|&k| ready(k)).unwrap_or_else(|| {
            cycle = true;
            0
        });
        order.push(left.remove(at));
    }
    let children: Vec<(&Object, &Object)> = order.iter().map(|&k| children[k]).collect();
    let mut slots: Vec<Option<(String, Vec<String>, Vec<String>, Option<Shape>)>> = carried.into_iter().map(Some).collect();
    let carried: Vec<(String, Vec<String>, Vec<String>, Option<Shape>)> = order.iter().filter_map(|&k| slots[k].take()).collect();
    let family: Vec<(Info, Info)> = std::iter::once((Info::of(from), Info::of(to))).chain(children.iter().map(|(x, y)| (Info::of(x), Info::of(y)))).collect();
    let changed = || carried.iter().filter_map(|(.., own)| own.as_ref());

    // What goes with them and the planner leaves be: the same on both sides, and on no column that
    // a change drops, which the planner replaces it for.
    let dropped: BTreeSet<String> = std::iter::once(&s).chain(changed()).flat_map(Shape::dropped).collect();
    let on_dropped = |id: &str| schemas.and_then(|(f, _)| f.objects.get(id)).is_some_and(|o| o.deps.iter().any(|d| dropped.contains(d)));
    let kept = |x: &(String, String, String, String, usize), other: &Info| other.goes.iter().any(|y| y.0 == x.0 && y.1 == x.1) && !on_dropped(&x.0);
    let mut going: BTreeMap<String, &(String, String, String, String, usize)> = BTreeMap::new();
    let mut coming: BTreeMap<String, &(String, String, String, String, usize)> = BTreeMap::new();
    for (x, y) in &family {
        for item in x.goes.iter().filter(|i| !i.2.is_empty() && kept(i, y)) {
            going.entry(item.0.clone()).or_insert(item);
        }
        for item in y.goes.iter().filter(|i| kept(i, x)) {
            coming.entry(item.0.clone()).or_insert(item);
        }
    }
    let by_rank = |items: BTreeMap<String, &(String, String, String, String, usize)>| {
        let mut items: Vec<(usize, String)> = items.values().map(|i| (i.4, i.3.clone())).collect();
        items.sort();
        items
    };
    let mut going: Vec<(usize, String)> = going.values().map(|i| (i.4, i.2.clone())).collect();
    going.sort();
    let mut out: Vec<String> = going.into_iter().rev().map(|(_, drop)| drop).collect();
    out.extend(std::iter::once(&s).chain(changed()).flat_map(Shape::warnings));
    let (copied, named) = (s.copied(), s.named());
    let texts: Vec<String> = std::iter::once(target.clone()).chain(carried.iter().map(|(text, ..)| text.clone())).collect();
    // A name no object of the schema has, nor one of `used`: `base`, else `base` and a number.
    let free = |base: String, used: &BTreeSet<String>| -> String {
        (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}_{n}") })
            .find(|n| !used.contains(&fold(n)) && !family.iter().any(|(x, y)| x.taken.contains(&fold(n)) || y.taken.contains(&fold(n))))
            .unwrap_or_default()
    };
    // With foreign keys on, as a Durable Object and D1 keep them, a row copied into a table that
    // references another looks its key up there, and a key that is a unique index of its own must
    // stand by then, not after the copies, where what goes with a table is otherwise made again:
    // the unique indexes of each rebuilt table that one of them references. One the plan replaces
    // (renames, or makes again another way) it drops before the rebuild and makes after it, so
    // the rebuild makes that one for itself, as it was, and drops it when the rows are in.
    let referenced: BTreeSet<String> = texts
        .iter()
        .flat_map(|text| {
            let tokens = tokenize(text, Dialect::Sqlite);
            (0..tokens.len()).filter(|&k| tokens[k].upper() == "REFERENCES").filter_map(|k| tokens.get(k + 1).and_then(qualifier)).collect::<Vec<_>>()
        })
        .filter(|table| family.iter().any(|(x, _)| fold(&x.name) == *table))
        .collect();
    // The columns each rebuilt table has while it is rebuilt: the new table's, by their old names.
    let columns_then: BTreeMap<String, BTreeSet<String>> = family.iter().zip(&texts).map(|((x, _), text)| (fold(&x.name), parts(text).iter().filter_map(|p| p.column.clone()).collect())).collect();
    // Whether a text is a key a foreign key can name: a unique index of a referenced table, on
    // whole columns it has then, over every row.
    let is_key = |create: &str| {
        let tokens = tokenize(create, Dialect::Sqlite);
        unique_index(&tokens).is_some_and(|(_, table)| name_of(&tokens[table]).is_some_and(|t| referenced.contains(&t) && columns_then.get(&t).is_some_and(|columns| whole_columns(&tokens[table + 1..], columns))))
    };
    let (keys, coming): (Vec<_>, Vec<_>) = by_rank(coming).into_iter().partition(|(_, create)| is_key(create));
    let mut replaced: Vec<(usize, String)> = Vec::new();
    for (x, y) in &family {
        for item in x.goes.iter().filter(|i| !kept(i, y) && is_key(&i.3)) {
            if !replaced.iter().any(|(_, create)| *create == item.3) {
                replaced.push((item.4, item.3.clone()));
            }
        }
    }
    replaced.sort();
    // A table whose text names its columns by its own name (a check written `"users"."age" > 21`)
    // can be neither made under another name nor renamed: SQLite reads the name as the table's
    // own. Where one of the tables rebuilt together is such, the rows of each wait in a table of
    // their own instead (no temporary one, which a Durable Object refuses), while the tables are
    // made again under their names, each after what it references, and filled in the same order.
    if family.iter().zip(&texts).any(|((x, _), text)| names_itself(text, &x.name)) {
        if cycle {
            out.push("-- these tables reference each other, so no order fills them: their keys wait for the commit".to_string());
            out.push("pragma defer_foreign_keys = on".to_string());
        }
        // Each member's columns to keep, the rowid where no column is it, and whether it counts.
        let mut members: Vec<(String, String, String, bool)> = Vec::new();
        let lists = std::iter::once((&copied, &named)).chain(carried.iter().map(|(_, copied, named, _)| (copied, named)));
        for (((x, y), text), (copied, names)) in family.iter().zip(&texts).zip(lists) {
            let mut kept = copied.clone();
            if x.bare_rowid && y.bare_rowid {
                if let Some(rowid) = ["rowid", "_rowid_", "oid"].into_iter().find(|n| !names.iter().any(|c| c == n)) {
                    kept.insert(0, rowid.to_string());
                }
            }
            members.push((x.name.clone(), kept.join(", "), text.clone(), x.autoincrement && y.autoincrement));
        }
        // Where each member's rows wait, and its counter: names no table has, nor another of these.
        let mut used: BTreeSet<String> = BTreeSet::new();
        let mut asides: Vec<(String, String)> = Vec::new();
        for (name, ..) in &members {
            let rows = free(format!("reylo_aside_{name}"), &used);
            used.insert(fold(&rows));
            let counter = free(format!("reylo_aside_{name}_sequence"), &used);
            used.insert(fold(&counter));
            asides.push((quote(&rows), quote(&counter)));
        }
        for ((name, list, _, sequence), (aside, seq_aside)) in members.iter().zip(&asides) {
            if !list.is_empty() {
                out.push(format!("create table {aside} as select {list} from {}", quote(name)));
            }
            if *sequence {
                out.push(format!("create table {seq_aside} as select seq from sqlite_sequence where name = {}", literal(name)));
            }
        }
        for (name, ..) in members.iter().rev() {
            out.push(format!("drop table {}", quote(name)));
        }
        for (_, _, text, _) in &members {
            out.push(text.clone());
        }
        // The old tables took their indexes with them: the keys come back before the rows, and
        // one the plan makes again itself stands meanwhile under a name of its own.
        out.extend(keys.into_iter().map(|(_, create)| create));
        let mut standing: Vec<String> = Vec::new();
        for (_, create) in &replaced {
            let tokens = tokenize(create, Dialect::Sqlite);
            let (name, _) = unique_index(&tokens)?;
            let scratch = free(format!("new_{}", unquote(&tokens[name])), &used);
            used.insert(fold(&scratch));
            out.push(rewrite(create, &tokens, |k, _| (k == name).then(|| quote(&scratch))));
            standing.push(scratch);
        }
        for ((name, list, _, sequence), (aside, seq_aside)) in members.iter().zip(&asides) {
            if !list.is_empty() {
                out.push(format!("insert into {} ({list}) select {list} from {aside}", quote(name)));
                out.push(format!("drop table {aside}"));
            }
            if *sequence {
                out.push(format!("delete from sqlite_sequence where name = {}", literal(name)));
                out.push(format!("insert into sqlite_sequence (name, seq) select {}, seq from {seq_aside}", literal(name)));
                out.push(format!("drop table {seq_aside}"));
            }
        }
        out.extend(standing.iter().map(|name| format!("drop index {}", quote(name))));
        out.extend(coming.into_iter().map(|(_, create)| create));
        out.extend(std::iter::once(&s).chain(changed()).flat_map(|s| s.reindexed(schemas)));
        return Some(out);
    }
    if cycle {
        out.push("-- these tables reference each other, so no order fills them: their keys wait for the commit".to_string());
        out.push("pragma defer_foreign_keys = on".to_string());
    }
    // Each table's new name, and the tables that reference one of them referencing that.
    let mut scratches: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (x, y) in &family {
        let name = (1..)
            .map(|n| if n == 1 { format!("new_{}", x.name) } else { format!("new_{}_{n}", x.name) })
            .find(|n| !x.taken.contains(&fold(n)) && !y.taken.contains(&fold(n)) && !scratches.values().any(|(s, _)| fold(s) == fold(n)))
            .unwrap_or_default();
        let quoted = quote(&name);
        scratches.insert(fold(&x.name), (name, quoted));
    }
    let scratch_of = |name: &str| scratches.get(&fold(name)).map(|(_, q)| q.clone()).unwrap_or_default();
    let renamed_text = |sql: &str, own: &str| -> Option<String> {
        let tokens = tokenize(sql, Dialect::Sqlite);
        let at = name_at(&tokens)?;
        let references: BTreeSet<usize> = tokens.iter().enumerate().filter(|(_, t)| t.upper() == "REFERENCES").map(|(k, _)| k + 1).collect();
        Some(rewrite(sql, &tokens, |k, t| {
            if k == at {
                return Some(scratch_of(own));
            }
            // A table a foreign key names, as a string too (`references 'users'`), which SQLite reads
            // as the name.
            if references.contains(&k) {
                return scratches.get(&qualifier(t)?).map(|(_, q)| q.clone());
            }
            None
        }))
    };
    out.push(renamed_text(&target, &a.name)?);
    for ((x, _), (text, ..)) in family[1..].iter().zip(&carried) {
        out.push(renamed_text(text, &x.name)?);
    }
    // Each key on its table's new one, under a name of its own while the old index has its name.
    let mut used: BTreeSet<String> = scratches.values().map(|(name, _)| fold(name)).collect();
    let mut passing: Vec<String> = Vec::new();
    for (_, create) in keys.iter().chain(&replaced) {
        let tokens = tokenize(create, Dialect::Sqlite);
        let (name, table) = unique_index(&tokens)?;
        let scratch = free(format!("new_{}", unquote(&tokens[name])), &used);
        used.insert(fold(&scratch));
        let on = scratch_of(&name_of(&tokens[table])?);
        out.push(rewrite(create, &tokens, |k, _| if k == name { Some(quote(&scratch)) } else if k == table { Some(on.clone()) } else { None }));
        passing.push(scratch);
    }
    copy_rows(&mut out, a, b, &scratches[&fold(&a.name)].0, copied, &named);
    for ((x, y), (_, copied, named, _)) in family[1..].iter().zip(&carried) {
        copy_rows(&mut out, x, y, &scratches[&fold(&x.name)].0, copied.clone(), named);
    }
    // Children first, so that no drop has a row left to fire an action on.
    for (x, _) in family.iter().rev() {
        out.push(format!("drop table {}", quote(&x.name)));
    }
    for (x, _) in &family {
        out.push(format!("alter table {} rename to {}", scratch_of(&x.name), quote(&x.name)));
    }
    // The keys under their own names again, as they were written, with the rest of what was taken down.
    out.extend(passing.iter().map(|name| format!("drop index {}", quote(name))));
    let mut again: Vec<(usize, String)> = keys.into_iter().chain(coming).collect();
    again.sort();
    out.extend(again.into_iter().map(|(_, create)| create));
    out.extend(std::iter::once(&s).chain(changed()).flat_map(|s| s.reindexed(schemas)));
    Some(out)
}

/// The statement that fills an external-content full-text table's index from its content table.
fn rebuild(name: &str) -> String {
    let q = quote(name);
    format!("insert into {q} ({q}) values ('rebuild')")
}

/// An FTS4 or FTS5 table's module, folded, and the arguments its text gives it, each its tokens.
fn fts_args(tokens: &[Token]) -> Option<(String, Vec<Vec<&Token>>)> {
    let using = tokens.iter().position(|t| t.upper() == "USING")?;
    let module = fold(&unquote(tokens.get(using + 1)?));
    if !matches!(module.as_str(), "fts4" | "fts5") || tokens.get(using + 2)?.text != "(" {
        return None;
    }
    let mut args: Vec<Vec<&Token>> = vec![Vec::new()];
    let mut depth = 0;
    for t in &tokens[using + 3..] {
        match t.text.as_str() {
            ")" if depth == 0 => break,
            "," if depth == 0 => {
                args.push(Vec::new());
                continue;
            }
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        if let Some(arg) = args.last_mut() {
            arg.push(t);
        }
    }
    args.retain(|a| !a.is_empty());
    Some((module, args))
}

/// An argument that sets an option (`content='notes'`): its name and its value, each folded.
fn option(arg: &[&Token]) -> Option<(String, String)> {
    arg.get(1).filter(|t| t.text == "=")?;
    Some((fold(&arg[0].text), fold(&arg[2..].iter().map(|t| unquote(t)).collect::<String>())))
}

/// What an FTS4 or FTS5 table's text says of the table whose text it indexes (`content='notes'`):
/// that table's name, the columns it reads there (FTS4's `languageid` among them), and the one its
/// index is keyed on (`content_rowid`, else the rowid), each folded. None for one that keeps its
/// own text, and for a contentless one (`content=''`), whose text is nowhere to rebuild it from.
fn external_content(tokens: &[Token]) -> Option<(String, Vec<String>, String)> {
    let (module, args) = fts_args(tokens)?;
    let (mut content, mut key, mut columns) = (None, "rowid".to_string(), Vec::new());
    for arg in &args {
        match option(arg) {
            Some((name, value)) => match name.as_str() {
                "content" => content = Some(value),
                // FTS4 keys its index on the content table's rowid, whatever its options say.
                "content_rowid" if module == "fts5" => key = value,
                "languageid" if module == "fts4" => columns.push(value),
                _ => {}
            },
            None => columns.push(fold(&unquote(arg[0]))),
        }
    }
    Some((content.filter(|c| !c.is_empty())?, columns, key))
}

/// The rebuilds of the external-content indexes over the table `a` (`b` in the target) that its
/// rebuild leaves out of step with their rows: where the key the index is read by is not copied
/// as it was, or a column it indexes may change its values in the copy (its definition, or the
/// table's options, changes). An index whose content is a view over the table is rebuilt whatever
/// changes, as the view's rows are not read here. Each runs once the plan's renames are done, and
/// not at all where the plan makes the index again (`finish`).
#[allow(clippy::too_many_arguments)]
fn reindexed(a: &Info, b: &Info, renamed: &BTreeMap<String, String>, old_parts: &[Part], new_parts: &[Part], named: &[String], reshaped: bool, schemas: Option<&(Schema, Schema)>) -> Vec<String> {
    let Some((from, to)) = schemas else { return Vec::new() };
    let table = fold(&a.name);
    let old_name = |c: &str| renamed.get(c).cloned().unwrap_or_else(|| c.to_string());
    let find = |ps: &[Part], c: &str| ps.iter().find(|p| p.column.as_deref() == Some(c)).map(Part::definition);
    // The rebuild's statements write each column by its name before the plan's renames. A
    // generated column's values change with any column it may read.
    let columns_same = old_parts.iter().chain(new_parts).filter_map(|p| p.column.as_deref()).all(|c| find(old_parts, c) == find(new_parts, c));
    let same = |c: &str| {
        let (was, is) = (find(old_parts, &old_name(c)), find(new_parts, &old_name(c)));
        was.is_some() && was == is && !reshaped && (!(b.flag(c, 'g') || a.flag(&old_name(c), 'g')) || columns_same)
    };
    // Whether each side has a column of the key's name, which the key then reads rather than the
    // rowid: the old table one so named, and the new one one renamed to it, or so named and not
    // renamed away.
    let old_has = |key: &str| find(old_parts, key).is_some();
    let new_has = |key: &str| renamed.contains_key(key) || (find(new_parts, key).is_some() && !renamed.values().any(|o| o == key));
    let kept = |key: &str| -> bool {
        match (old_has(key), new_has(key)) {
            // The same column, copied as it was.
            (true, true) => old_name(key) == key && same(key),
            (false, false) if matches!(key, "rowid" | "oid" | "_rowid_") => {
                // The rowid is copied where neither table has a column that is it, and is the
                // column that is it, copied, where both have that one.
                let copied = a.bare_rowid && b.bare_rowid && ["rowid", "_rowid_", "oid"].into_iter().any(|n| !named.iter().any(|c| c == n));
                let aliased = b.alias.as_deref().is_some_and(|c| a.alias.as_deref() == Some(old_name(c).as_str()));
                copied || aliased
            }
            _ => false,
        }
    };
    // Whether a view of the target reads the table, through other views or not.
    let over_table = |view: &str| -> bool {
        let (mut queue, mut seen) = (vec![view.to_string()], BTreeSet::new());
        while let Some(id) = queue.pop() {
            let Some(o) = to.objects.get(&id).filter(|o| o.kind == Some(Kind::View) && seen.insert(id.clone())) else { continue };
            if o.deps.contains(&format!("table {table}")) {
                return true;
            }
            queue.extend(o.deps.iter().cloned());
        }
        false
    };
    let unchanged = |id: &str| from.objects.get(id).zip(to.objects.get(id)).is_some_and(|(f, t)| f.def == t.def);
    to.objects
        .values()
        .filter(|o| unchanged(&o.id))
        .filter_map(|o| {
            let info = Info::of(o);
            let (content, key, read) = info.content.as_ref()?;
            let stale = if *content == table { !kept(key) || !read.iter().all(|c| same(c)) } else { over_table(&format!("view {content}")) };
            (stale && o.create.len() > 1).then(|| rebuild(&info.name))
        })
        .collect()
}

/// What a view's text reads, folded. Loosely, every name it holds, which takes in an alias, a
/// literal or a CTE named like a relation but misses no read. Otherwise each source its FROM
/// clauses name: a table-valued call (`join 'notes_fts'('word')`) among them, the name after a
/// schema (`main.notes_fts`), the first of a parenthesized join, and never a CTE's name.
fn view_reads(sql: &str, loose: bool) -> Vec<String> {
    let tokens = tokenize(sql, Dialect::Sqlite);
    let named = |k: usize| tokens.get(k).is_some_and(|t| matches!(t.kind, TokenKind::Word | TokenKind::Quoted | TokenKind::Str));
    if loose {
        return (0..tokens.len()).filter(|&k| named(k)).map(|k| fold(&unquote(&tokens[k]))).collect();
    }
    // How deep in parentheses each token is.
    let depths: Vec<usize> = tokens
        .iter()
        .scan(0usize, |depth, t| {
            let at = *depth;
            match t.text.as_str() {
                "(" => *depth += 1,
                ")" => *depth = depth.saturating_sub(1),
                _ => {}
            }
            Some(if t.text == ")" { *depth } else { at })
        })
        .collect();
    // The names its WITH clauses give (one before `as (`, or before its column list and that),
    // each with the tokens it is bound over: from its WITH, which binds its siblings' bodies too,
    // to the end of the parentheses that hold it.
    // The WITH that opens the list a name at `k` would be in: the last at its depth, with no
    // SELECT at that depth since, which would have ended the list.
    let list_of = |k: usize| -> Option<usize> {
        let at = (0..k).rev().find(|&j| depths[j] == depths[k] && matches!(tokens[j].upper().as_str(), "WITH" | "SELECT"))?;
        (tokens[at].upper() == "WITH").then_some(at)
    };
    let ctes: Vec<(String, usize, usize)> = (0..tokens.len())
        .filter(|&k| named(k) && k > 0 && (matches!(tokens[k - 1].upper().as_str(), "WITH" | "RECURSIVE") || tokens[k - 1].text == ","))
        .filter_map(|k| {
            let mut j = k + 1;
            if tokens.get(j).is_some_and(|t| t.text == "(") {
                j = (j..tokens.len()).find(|&i| tokens[i].text == ")").map_or(tokens.len(), |i| i + 1);
            }
            // `as (`, `as materialized (` or `as not materialized (`.
            let word = |i: usize, w: &str| tokens.get(i).is_some_and(|t| t.upper() == w);
            let body = if word(j + 1, "NOT") { j + 3 } else if word(j + 1, "MATERIALIZED") { j + 2 } else { j + 1 };
            let declared = word(j, "AS") && (!word(j + 1, "NOT") || word(j + 2, "MATERIALIZED")) && tokens.get(body).is_some_and(|t| t.text == "(");
            if !declared {
                return None;
            }
            let with = list_of(k)?;
            Some((fold(&unquote(&tokens[k])), with, (k..tokens.len()).find(|&j| depths[j] < depths[k]).unwrap_or(tokens.len())))
        })
        .collect();
    let bound = |name: &str, at: usize| ctes.iter().any(|(n, start, end)| n == name && (*start..*end).contains(&at));
    // Whether each depth of parentheses is in a FROM clause.
    let mut from: Vec<bool> = vec![false];
    let mut out = Vec::new();
    for (k, t) in tokens.iter().enumerate() {
        let prev = k.checked_sub(1).map(|j| &tokens[j]);
        // A source: what follows FROM, JOIN, a comma in a FROM clause, or the parenthesis that
        // opens a grouped join there.
        let source = |p: &Token, from: &[bool]| matches!(p.upper().as_str(), "FROM" | "JOIN") || ((p.text == "," || p.text == "(") && from.last() == Some(&true));
        match t.text.as_str() {
            "(" => {
                let grouped = prev.is_some_and(|p| source(p, &from));
                from.push(grouped);
            }
            ")" => {
                from.pop();
                if from.is_empty() {
                    from.push(false);
                }
            }
            _ => {}
        }
        let here = from.len() - 1;
        match t.upper().as_str() {
            "FROM" => from[here] = true,
            "WHERE" | "GROUP" | "HAVING" | "ORDER" | "LIMIT" | "WINDOW" | "UNION" | "INTERSECT" | "EXCEPT" | "SELECT" | "VALUES" | "WITH" => from[here] = false,
            _ => {}
        }
        if named(k) && prev.is_some_and(|p| source(p, &from)) {
            let at = if tokens.get(k + 1).is_some_and(|n| n.text == ".") && named(k + 2) { k + 2 } else { k };
            let name = fold(&unquote(&tokens[at]));
            if at != k || !bound(&name, k) {
                out.push(name);
            }
        }
    }
    out
}

/// The FTS5 table an `fts5vocab` table's text reads, folded: its first argument, or its second
/// where the first names a schema.
fn vocabulary_of(sql: &str) -> Option<String> {
    let tokens = tokenize(sql, Dialect::Sqlite);
    let using = tokens.iter().position(|t| t.upper() == "USING")?;
    if fold(&unquote(tokens.get(using + 1)?)) != "fts5vocab" {
        return None;
    }
    let args: Vec<String> = tokens[using + 2..].iter().filter(|t| matches!(t.kind, TokenKind::Word | TokenKind::Quoted | TokenKind::Str)).map(|t| fold(&unquote(t))).collect();
    args.get(if args.len() > 2 { 1 } else { 0 }).cloned()
}

/// The full-text table a statement rebuilds the index of (`rebuild`'s), folded.
fn rebuilt(statement: &str) -> Option<String> {
    let tokens = tokenize(statement, Dialect::Sqlite);
    let words: Vec<String> = tokens.iter().map(|t| t.text.to_ascii_lowercase()).collect();
    (words.len() == 10 && words[0] == "insert" && words[1] == "into" && words[8] == "'rebuild'" && name_of(&tokens[2]) == name_of(&tokens[4])).then(|| name_of(&tokens[2])).flatten()
}


/// Where a unique index's stored text names the index and its table; None for anything else.
fn unique_index(tokens: &[Token]) -> Option<(usize, usize)> {
    let at = name_at(tokens)?;
    let unique = tokens[..at].iter().any(|t| t.upper() == "UNIQUE") && tokens[at - 1].upper() == "INDEX";
    (unique && tokens.get(at + 1).is_some_and(|t| t.upper() == "ON") && at + 2 < tokens.len()).then_some((at, at + 2))
}

/// Whether what follows an index's table in its text is a list of whole columns among `columns`
/// and nothing more: no expression and no WHERE, neither of which a foreign key's parent key may
/// have. A column may name its collation, which is the column's own where the index is a key.
fn whole_columns(list: &[Token], columns: &BTreeSet<String>) -> bool {
    let Some(close) = list.iter().position(|t| t.text == ")") else { return false };
    if list.first().is_none_or(|t| t.text != "(") || close + 1 != list.len() {
        return false;
    }
    let mut k = 1;
    while k < close {
        let t = &list[k];
        // A collation, and its name after it.
        if t.upper() == "COLLATE" {
            k += 2;
            continue;
        }
        if t.text != "," && !matches!(t.upper().as_str(), "ASC" | "DESC") && name_of(t).is_none_or(|c| !columns.contains(&c)) {
            return false;
        }
        k += 1;
    }
    true
}

/// The names in the parenthesized list `tokens` opens with, folded and in order, each with the
/// collation it names, uppercased, but for its order: None where it holds anything but names.
fn listed(tokens: &[Token]) -> Option<(Vec<(String, Option<String>)>, usize)> {
    if tokens.first()?.text != "(" {
        return None;
    }
    let close = tokens.iter().position(|t| t.text == ")")?;
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut k = 1;
    while k < close {
        let t = &tokens[k];
        if t.upper() == "COLLATE" {
            out.last_mut()?.1 = Some(unquote(tokens.get(k + 1)?).to_uppercase());
            k += 2;
            continue;
        }
        if t.text != "," && !matches!(t.upper().as_str(), "ASC" | "DESC") {
            out.push((name_of(t)?, None));
        }
        k += 1;
    }
    Some((out, close))
}

/// The primary key of a table's text, its columns folded, in their order.
fn primary_key(sql: &str) -> Option<Vec<String>> {
    parts(sql).into_iter().find_map(|p| {
        if !p.words().windows(2).any(|w| w[0] == "PRIMARY" && w[1] == "KEY") {
            return None;
        }
        match &p.column {
            Some(c) => Some(vec![c.clone()]),
            None => p.tokens.iter().position(|t| t.text == "(").and_then(|at| listed(&p.tokens[at..])).map(|(cols, _)| cols.into_iter().map(|(c, _)| c).collect()),
        }
    })
}

/// The keys a foreign key can name in table `table` of `schema`, each as its columns, folded: its
/// primary key, each UNIQUE, and each unique index on whole columns over every row, where each
/// column's collation is the one the column declares, as a parent key's must be.
fn keys_of(schema: &Schema, table: &Object) -> Vec<BTreeSet<String>> {
    let parts = table.create.first().map(|sql| parts(sql)).unwrap_or_default();
    let declared: BTreeMap<String, String> = parts
        .iter()
        .filter_map(|p| {
            // Its own COLLATE, the last outside parentheses: one in a CHECK is the expression's.
            let mut depth = 0;
            let at = p.tokens.iter().enumerate().filter(|(_, t)| {
                depth += i32::from(t.text == "(") - i32::from(t.text == ")");
                depth == 0 && t.upper() == "COLLATE"
            }).map(|(k, _)| k).last()?;
            Some((p.column.clone()?, unquote(p.tokens.get(at + 1)?).to_uppercase()))
        })
        .collect();
    let key = |cols: Vec<(String, Option<String>)>| -> Option<BTreeSet<String>> {
        let own = |c: &str, named: &str| declared.get(c).map_or("BINARY", String::as_str) == named;
        cols.iter().all(|(c, named)| named.as_deref().is_none_or(|n| own(c, n))).then(|| cols.into_iter().map(|(c, _)| c).collect())
    };
    let mut out = Vec::new();
    for p in &parts {
        let words = p.words();
        let keyed = words.iter().any(|w| w == "UNIQUE") || words.windows(2).any(|w| w[0] == "PRIMARY" && w[1] == "KEY");
        match &p.column {
            Some(c) if keyed => out.push(BTreeSet::from([c.clone()])),
            None if keyed => {
                let at = p.tokens.iter().position(|t| t.text == "(");
                out.extend(at.and_then(|at| listed(&p.tokens[at..])).and_then(|(cols, _)| key(cols)));
            }
            _ => {}
        }
    }
    for index in schema.objects.values().filter(|o| o.kind == Some(Kind::Index) && o.deps.contains(&table.id)) {
        let tokens = tokenize(index.create.first().map_or("", String::as_str), Dialect::Sqlite);
        if let Some((_, on)) = unique_index(&tokens) {
            let list = &tokens[on + 1..];
            out.extend(listed(list).filter(|(_, close)| close + 1 == list.len()).and_then(|(cols, _)| key(cols)));
        }
    }
    out
}

/// Whether `text`, a table's, references table `parent` (as the target has it) by a key it has yet
/// to have: a table `from` lacks, columns no key of `from`'s covers, or, naming no columns, a primary
/// key the change gives it or moves.
fn new_key(text: &str, from: &Schema, parent: &Object) -> bool {
    let Some(old) = from.objects.get(&parent.id) else { return true };
    let (keys, name) = (keys_of(from, old), fold(&Info::of(parent).name));
    let primary = || old.create.first().and_then(|sql| primary_key(sql)).is_none_or(|was| parent.create.first().and_then(|sql| primary_key(sql)) != Some(was));
    let tokens = tokenize(text, Dialect::Sqlite);
    (0..tokens.len()).filter(|&k| tokens[k].upper() == "REFERENCES" && tokens.get(k + 1).and_then(qualifier).as_deref() == Some(name.as_str())).any(|k| match tokens.get(k + 2).filter(|t| t.text == "(") {
        Some(_) => listed(&tokens[k + 2..]).is_none_or(|(cols, _)| !keys.contains(&cols.into_iter().map(|(c, _)| c).collect())),
        None => primary(),
    })
}

/// Whether a table's text names a column by the table's own name, as `"users"."age"`.
fn names_itself(sql: &str, table: &str) -> bool {
    let tokens = tokenize(sql, Dialect::Sqlite);
    // The table's name before a dot, after its schema's (`main.users.age`) or not.
    let first = |k: usize| k == 0 || tokens[k - 1].text != "." || (k >= 2 && qualifier(&tokens[k - 2]).is_some() && (k < 3 || tokens[k - 3].text != "."));
    (0..tokens.len()).any(|k| tokens.get(k + 1).is_some_and(|n| n.text == ".") && first(k) && qualifier(&tokens[k]).is_some_and(|n| n == fold(table)))
}

/// The name a qualifier before a dot says, folded: a word, a quoted name, or a string, which
/// SQLite reads as a name there (`'users'.age`).
fn qualifier(t: &Token) -> Option<String> {
    matches!(t.kind, TokenKind::Word | TokenKind::Quoted | TokenKind::Str).then(|| fold(&unquote(t)))
}

/// The tables of `from` that reference the table `id`, at any depth, and still do in `to`, each with
/// itself in `to`: one the plan drops is no part of a rebuild, since the plan drops it first.
fn children_of<'a>(from: &'a Schema, to: &'a Schema, id: &str) -> Vec<(&'a Object, &'a Object)> {
    let table = |o: &&Object| o.kind == Some(Kind::Table) && !Info::of(o).virtual_table && to.objects.contains_key(&o.id);
    let mut family: Vec<(&Object, &Object)> = Vec::new();
    let mut seen: Vec<String> = vec![id.to_string()];
    let mut k = 0;
    while k < seen.len() {
        let parent = seen[k].clone();
        k += 1;
        // One that stops referencing it changes first, the planner's order for what stops
        // depending on a table, so it no longer references it when the rebuild runs.
        let still = |o: &&Object| to.objects.get(&o.id).is_some_and(|t| t.deps.contains(&parent));
        for o in from.objects.values().filter(table).filter(|o| o.deps.contains(&parent)).filter(still) {
            if !seen.contains(&o.id) {
                seen.push(o.id.clone());
                family.push((o, &to.objects[&o.id]));
            }
        }
    }
    family
}

/// The tables whose rebuild makes table `id` again with them (`children_of`, the other way round): those
/// it references at any depth, through tables that still reference the next in `to`.
fn parents_of(from: &Schema, to: &Schema, id: &str) -> Vec<String> {
    let table = |id: &str| from.objects.get(id).is_some_and(|o| o.kind == Some(Kind::Table) && !Info::of(o).virtual_table);
    let mut seen: Vec<String> = vec![id.to_string()];
    let mut k = 0;
    while k < seen.len() {
        let child = seen[k].clone();
        k += 1;
        let (Some(x), Some(y)) = (from.objects.get(&child), to.objects.get(&child)) else { continue };
        for d in x.deps.iter().filter(|d| y.deps.contains(*d) && table(d)) {
            if !seen.contains(d) {
                seen.push(d.clone());
            }
        }
    }
    seen.split_off(1)
}

/// The tables that table `id` references and that reference it back, at any depth, as the target
/// has them, `id` among them, in the order of their ids: tables on both sides, which a rebuild takes
/// to the target together, references they come to have included.
fn circle(from: &Schema, to: &Schema, id: &str) -> Vec<String> {
    let table = |x: &str| from.objects.get(x).zip(to.objects.get(x)).is_some_and(|(a, b)| b.kind == Some(Kind::Table) && !Info::of(a).virtual_table && !Info::of(b).virtual_table);
    let reach = |up: bool| -> BTreeSet<String> {
        let (mut seen, mut queue) = (BTreeSet::from([id.to_string()]), vec![id.to_string()]);
        while let Some(x) = queue.pop() {
            let next: Vec<String> = if up { to.objects.get(&x).map(|o| o.deps.iter().cloned().collect()).unwrap_or_default() } else { to.objects.values().filter(|o| o.deps.contains(&x)).map(|o| o.id.clone()).collect() };
            for n in next.into_iter().filter(|n| table(n)) {
                if seen.insert(n.clone()) {
                    queue.push(n);
                }
            }
        }
        seen
    };
    reach(true).intersection(&reach(false)).cloned().collect()
}

/// The table whose rebuild takes table `id` to the target, where its own change is a rebuild or one
/// of the tables it references back rebuilds: of those (`circle`), the first whose change is one.
fn rebuilt_by(from: &Schema, to: &Schema, id: &str, decided: Renamed) -> Option<String> {
    let rebuilds = |t: &str| -> Option<bool> { Some(shape(from.objects.get(t)?, to.objects.get(t)?, decided(t.strip_prefix("table ")?))?.rebuild) };
    circle(from, to, id).into_iter().find(|t| rebuilds(t) == Some(true))
}

/// What copies a table's rows into its rebuild, named `scratch`: the columns `copied` names, its
/// rowid where no column is it, and AUTOINCREMENT's counter, which can be past every row's key.
fn copy_rows(out: &mut Vec<String>, a: &Info, b: &Info, scratch: &str, mut copied: Vec<String>, named: &[String]) {
    let (table, quoted) = (quote(&a.name), quote(scratch));
    if a.bare_rowid && b.bare_rowid {
        if let Some(rowid) = ["rowid", "_rowid_", "oid"].into_iter().find(|n| !named.iter().any(|c| c == n)) {
            copied.insert(0, rowid.to_string());
        }
    }
    if !copied.is_empty() {
        let list = copied.join(", ");
        out.push(format!("insert into {quoted} ({list}) select {list} from {table}"));
    }
    if a.autoincrement && b.autoincrement {
        out.push(format!("delete from sqlite_sequence where name = {}", literal(scratch)));
        out.push(format!("insert into sqlite_sequence (name, seq) select {}, seq from sqlite_sequence where name = {}", literal(scratch), literal(&a.name)));
    }
}

/// A table's text with each column the planner renames written by its old name, as the table is
/// until the rename runs: in the column list, where a check names it by the table's own name
/// (`t.new`), and in a foreign key to the table itself, but not where a foreign key names another
/// table's columns, nor a function.
fn before_renames(sql: &str, renamed: &BTreeMap<String, String>, written_as: &BTreeMap<String, String>) -> String {
    if renamed.is_empty() {
        return sql.to_string();
    }
    let tokens = tokenize(sql, Dialect::Sqlite);
    let open = tokens.iter().position(|t| t.text == "(").unwrap_or(tokens.len());
    let own = name_at(&tokens).and_then(|at| qualifier(&tokens[at]));
    // A name after a dot that isn't the table's own is no column of it, and a name before one
    // is a table's or a schema's.
    let qualified_elsewhere = |k: usize| tokens.get(k.wrapping_sub(1)).is_some_and(|p| p.text == ".") && tokens.get(k.wrapping_sub(2)).and_then(qualifier) != own;
    let qualifies = |k: usize| tokens.get(k + 1).is_some_and(|n| n.text == ".");
    let mut skip = BTreeSet::new();
    for (k, t) in tokens.iter().enumerate() {
        // The table a foreign key names is no column.
        if t.upper() == "REFERENCES" {
            skip.insert(k + 1);
        }
        // Another table's columns, but not those of a foreign key to the table itself.
        if t.upper() == "REFERENCES" && tokens.get(k + 1).and_then(qualifier) != own {
            let mut j = k + 2;
            if tokens.get(j).is_some_and(|t| t.text == "(") {
                while j < tokens.len() && tokens[j].text != ")" {
                    skip.insert(j);
                    j += 1;
                }
            }
        }
    }
    rewrite(sql, &tokens, |k, t| {
        if k <= open || skip.contains(&k) || tokens.get(k + 1).is_some_and(|n| n.text == "(") || qualified_elsewhere(k) || qualifies(k) {
            return None;
        }
        // After the table's own name and a dot, a string names a column too (`t.'new'`).
        let after_dot = tokens.get(k.wrapping_sub(1)).is_some_and(|p| p.text == ".");
        let old = renamed.get(&(if after_dot { qualifier(t) } else { name_of(t) })?)?;
        Some(written_as.get(old).cloned().unwrap_or_else(|| quote(old)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_as_the_comparator_does() {
        assert_eq!(canonical(&tokenize("CREATE INDEX \"I\" ON t ( a , b )", Dialect::Sqlite)), "create index i on t (a, b)");
    }

    #[test]
    fn splits_a_table_into_columns_and_constraints() {
        let p = parts("CREATE TABLE t (id integer primary key, \"Full Name\" text check (length(\"Full Name\") > 0), constraint k unique (id, \"Full Name\"))");
        assert_eq!(p.iter().map(|p| p.column.clone()).collect::<Vec<_>>(), [Some("id".into()), Some("full name".into()), None]);
        assert_eq!(p[1].definition(), "text check (length (full name) > 0)");
    }

    #[test]
    fn reads_whether_a_default_is_a_constant() {
        let d = |text: &str| default_of(&parts(&format!("create table t ({text})"))[0]);
        assert_eq!(d("a int"), None);
        assert_eq!(d("a int default 1"), Some((true, false)));
        assert_eq!(d("a int default -1 not null"), Some((true, false)));
        assert_eq!(d("a blob default x'00'"), Some((true, false)));
        assert_eq!(d("a int default null"), Some((true, true)));
        assert_eq!(d("a int default (1)"), Some((true, false)));
        assert_eq!(d("a text default current_timestamp"), Some((false, false)));
        assert_eq!(d("a text default ('a' || 'b')"), Some((false, false)));
    }

    #[test]
    fn writes_a_table_by_its_columns_old_names() {
        let renamed = BTreeMap::from([("name".to_string(), "full_name".to_string())]);
        let written_as = BTreeMap::from([("full_name".to_string(), "full_name".to_string())]);
        let sql = "CREATE TABLE name (id integer primary key, name text check (length(name) > 0), boss text references people (name))";
        assert_eq!(before_renames(sql, &renamed, &written_as), "CREATE TABLE name (id integer primary key, full_name text check (length(full_name) > 0), boss text references people (name))");
        let sql = "CREATE TABLE t (name text, boss text references t (name), check (t.name <> ''), check (main.\"T\".name <> ''), check ('t'.name <> ''))";
        assert_eq!(before_renames(sql, &renamed, &written_as), "CREATE TABLE t (full_name text, boss text references t (full_name), check (t.full_name <> ''), check (main.\"T\".full_name <> ''), check ('t'.full_name <> ''))");
        // A column renamed to the table's name leaves the qualifier be.
        let to_t = BTreeMap::from([("t".to_string(), "old".to_string())]);
        assert_eq!(before_renames("CREATE TABLE t (t integer, check (t.t > 0))", &to_t, &BTreeMap::new()), "CREATE TABLE t (old integer, check (t.old > 0))");
        assert_eq!(before_renames("CREATE TABLE t (t integer primary key, p integer references t, check (t.'t' > 0 and p <> 't'))", &to_t, &BTreeMap::new()), "CREATE TABLE t (old integer primary key, p integer references t, check (t.old > 0 and p <> 't'))");
        assert!(names_itself("create table u (old integer check (main.u.old > 0))", "u") && names_itself("create table u (x check ('u'.x))", "u") && !names_itself("create table u (x check (main.x > 0))", "u"));
    }

    #[test]
    fn reads_what_a_full_text_table_indexes() {
        let content = |sql: &str| external_content(&tokenize(sql, Dialect::Sqlite));
        let notes = |columns: &[&str], key: &str| Some(("notes".to_string(), columns.iter().map(|c| c.to_string()).collect(), key.to_string()));
        assert_eq!(content("CREATE VIRTUAL TABLE f USING fts5(body, content='notes', content_rowid='id', tokenize='porter')"), notes(&["body"], "id"));
        assert_eq!(content("create virtual table f using FTS5(Title, body UNINDEXED, content = \"Notes\")"), notes(&["title", "body"], "rowid"));
        assert_eq!(content("create virtual table f using fts4(body, content=notes, tokenize=porter)"), notes(&["body"], "rowid"));
        assert_eq!(content("create virtual table f using fts5(body, content='')"), None);
        assert_eq!(content("create virtual table f using fts5(body, tokenize='unicode61 remove_diacritics 2')"), None);
        assert_eq!(content("create virtual table f using rtree(id, x0, x1)"), None);
    }

    #[test]
    fn reads_the_relations_a_view_reads() {
        let read = |sql: &str| view_reads(sql, false);
        assert_eq!(read("create view v as select f.id, 'filtered' as kind from notes as f join main.\"Z_Root\"('word') as hit on hit.rowid = f.id, other o where f.category <> 'filtered'"), ["notes", "z_root", "other"]);
        assert_eq!(read("create view v as select a, filtered from (select x as a, y as filtered from t1, t2) join 'docs'('x')"), ["t1", "t2", "docs"]);
        assert_eq!(read("create view v as with filtered as (select id from notes), c (x) as materialized (select 1) select * from (z_root join filtered on 1) join c"), ["notes", "z_root"]);
        assert_eq!(read("create view v as select * from a join z_root on 1 where exists (with z_root as (select 1) select * from z_root)"), ["a", "z_root"]);
        assert_eq!(read("create view v as with src as (select id from filtered), filtered as (select id from z_notes) select * from src"), ["z_notes"]);
        assert_eq!(read("create view v as with src as (select id from a) select * from src join z_root on 1, z_root('word') as materialized"), ["a", "z_root", "z_root"]);
        assert_eq!(vocabulary_of("CREATE VIRTUAL TABLE v USING fts5vocab(z_root, 'instance')"), Some("z_root".to_string()));
        assert_eq!(vocabulary_of("create virtual table v using fts5vocab(main, \"Docs\", row)"), Some("docs".to_string()));
        assert_eq!(vocabulary_of("create virtual table v using fts5(body)"), None);
    }

    #[test]
    fn lowercases_what_sqlite_writes_before_the_name() {
        assert_eq!(written("CREATE TABLE t (a int)"), "create table t (a int)");
        assert_eq!(written("CREATE UNIQUE INDEX I ON T (A)"), "CREATE UNIQUE INDEX I ON T (A)");
        assert_eq!(quote("my table"), "\"my table\"");
        assert_eq!(quote("order"), "\"order\"");
        assert_eq!(quote("users"), "users");
    }
}
