//! MySQL's schema as the diff engine's objects. Every definition is the server's own rendering
//! (`show create table`, `show create view`, `show create function`, …), tidied into the way a
//! person writes it: keywords in lowercase, names quoted only where they must be, the database's
//! own name taken out (a shadow is another database on the same server, so a plan names nothing
//! with its database), and the version comments the server wraps around newer clauses opened.
//! The dependencies are the server's records where it keeps them (foreign keys,
//! `view_table_usage`, `view_routine_usage`) and a generated column's own expression; the objects
//! are those the comparator reads, with the history tables left out, Reylo's own and other tools'.
//!
//! MySQL commits every DDL statement on its own, so a plan changes each table in one `alter table`
//! with a clause per change. A table is therefore one object whose definition is its whole body
//! (its options, columns, indexes and checks) and whose alterer writes that one statement; its
//! columns are objects of their own too, which carry no statement, so that the planner sees what
//! is dropped, renamed and depended on. A foreign key names another table, so it is a statement
//! of its own, after both tables are as it needs them. Views change in place with `create or
//! replace`, routines and events are dropped and created again, and a trigger, whose place among
//! its table's triggers is part of it, is replaced with every trigger after it.

use super::{Alterer, Change, Kind, Object, Schema};
use crate::catalog::not_history;
use crate::db::{Db, Row};
use crate::sql::{tokenize, Token, TokenKind};
use crate::{Dialect, Error};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

fn backtick(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// What the reader knows of the server: the database's name (taken out of every definition), its
/// reserved words (names that must stay quoted), the account it reads as (a definer that is this
/// account is left out of what it writes) and the defaults a table takes when it names none.
struct Ctx {
    db: String,
    reserved: BTreeSet<String>,
    user: String,
    engine: String,
    collation: String,
}

/// How `Ctx::tidy` rewrites a text.
#[derive(Clone, Copy)]
struct Tidy {
    /// Keywords lowercased: only for the server's own renderings, where every name is quoted.
    lower: bool,
    /// Names unquoted where they need no quotes.
    unquote: bool,
    /// Runs of whitespace collapsed to one space, and a space after each comma.
    collapse: bool,
    /// Whether a name right before a parenthesis may be a stored function's, which stays quoted
    /// so that it is never read as a built-in of its name (a view's `count`); in a table's own
    /// elements it is a column with a prefix length (`name(10)`), and in a routine's head the
    /// routine's own name, since neither calls a stored function.
    calls: bool,
}

/// The built-in functions MySQL's parser reads by their names wherever a parenthesis follows, so
/// that such a name before one is always a call, even where a name is due (`create function
/// count(…)` is refused).
const PARSED_CALLS: &[&str] = &[
    "ADDDATE", "BIT_AND", "BIT_OR", "BIT_XOR", "CAST", "COUNT", "CURDATE", "CURTIME", "DATE_ADD", "DATE_SUB", "EXTRACT", "GROUP_CONCAT", "MAX", "MID", "MIN", "NOW", "POSITION", "SESSION_USER", "STD", "STDDEV",
    "STDDEV_POP", "STDDEV_SAMP", "SUBDATE", "SUBSTR", "SUBSTRING", "SUM", "SYSDATE", "SYSTEM_USER", "TRIM", "VARIANCE", "VAR_POP", "VAR_SAMP",
];

const SERVER_TEXT: Tidy = Tidy { lower: true, unquote: true, collapse: true, calls: true };
const TABLE_TEXT: Tidy = Tidy { calls: false, ..SERVER_TEXT };
const BODY_TEXT: Tidy = Tidy { lower: false, unquote: false, collapse: false, calls: true };

impl Ctx {
    /// A name as a statement writes it: bare where MySQL reads it bare as the same name.
    fn ident(&self, name: &str) -> String {
        let plain = name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
        if plain && !self.reserved.contains(&name.to_ascii_uppercase()) {
            name.to_string()
        } else {
            backtick(name)
        }
    }

    /// `text` with the database's own name taken out of every qualified name, and rewritten as
    /// `how` says; string literals and comments are kept as they are.
    fn tidy(&self, text: &str, how: Tidy) -> String {
        let text = open_version_comments(text);
        let tokens = tokenize(&text, Dialect::Mysql);
        let mut out = String::new();
        let mut at = 0;
        let mut k = 0;
        while k < tokens.len() {
            let t = &tokens[k];
            let gap = &text[at..t.start];
            if how.collapse && gap.trim().is_empty() {
                let after_comma = k > 0 && tokens[k - 1].text == "," && !matches!(t.text.as_str(), ")" | ",");
                if !gap.is_empty() || after_comma {
                    out.push(' ');
                }
            } else {
                out.push_str(gap);
            }
            let next = tokens.get(k + 1);
            let name = name_of(t);
            // `db`.`t` and db.t: the database's own name and its dot go.
            if name.as_deref() == Some(self.db.as_str()) && next.is_some_and(|n| n.text == ".") && tokens.get(k + 2).is_some_and(|n| name_of(n).is_some()) {
                k += 2;
                // The next name follows the dropped qualifier with no gap.
                let t = &tokens[k];
                out.push_str(&self.token(t, tokens.get(k + 1), how));
                at = t.end;
                k += 1;
                continue;
            }
            out.push_str(&self.token(t, next, how));
            at = t.end;
            k += 1;
        }
        out.push_str(if how.collapse { text[at..].trim_end() } else { &text[at..] });
        if how.collapse {
            out.trim().to_string()
        } else {
            out
        }
    }

    fn token(&self, t: &Token, next: Option<&Token>, how: Tidy) -> String {
        match t.kind {
            TokenKind::Quoted if t.text.starts_with('`') && how.unquote && !(next.is_some_and(|n| n.text == "(" && n.start == t.end) && (how.calls || PARSED_CALLS.contains(&name_of(t).unwrap_or_default().to_ascii_uppercase().as_str()))) => {
                let name = name_of(t).unwrap_or_default();
                self.ident(&name)
            }
            TokenKind::Word if how.lower => t.text.to_ascii_lowercase(),
            _ => t.text.clone(),
        }
    }

    /// The definer as a clause, or nothing where it is the account that reads.
    fn definer(&self, definer: &str) -> String {
        if definer.is_empty() || definer == self.user {
            return String::new();
        }
        match definer.rsplit_once('@') {
            Some((user, host)) => format!("definer = {}@{} ", backtick(user), backtick(host)),
            None => format!("definer = {} ", backtick(definer)),
        }
    }
}

/// The name a token is: a bare word, or a backquoted name unquoted.
fn name_of(t: &Token) -> Option<String> {
    match t.kind {
        TokenKind::Word => Some(t.text.clone()),
        TokenKind::Quoted if t.text.starts_with('`') && t.text.len() >= 2 => Some(t.text[1..t.text.len() - 1].replace("``", "`")),
        _ => None,
    }
}

/// The server's version comments (`/*!80023 INVISIBLE */`) opened into the clauses they hold: the
/// plan runs on the server that wrote them.
fn open_version_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' && q != b'`' {
                i += 2;
                continue;
            }
            if c == q {
                if b.get(i + 1) == Some(&q) {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if matches!(c, b'\'' | b'"' | b'`') {
            quote = Some(c);
            i += 1;
            continue;
        }
        if text[i..].starts_with("/*!") {
            let mut j = i + 3;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let Some(end) = text[j..].find("*/").map(|e| j + e) else { break };
            out.push_str(&text[copied..i]);
            out.push_str(text[j..end].trim());
            i = end + 2;
            copied = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&text[copied.min(text.len())..]);
    out
}

/// `text` with the byte ranges `cut` left out, each with the whitespace before it.
fn cut(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for &(start, end) in ranges {
        out.push_str(text[at..start].trim_end());
        at = end;
    }
    out.push_str(&text[at..]);
    out.trim().to_string()
}

/// A column's definition, read back from its parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Column {
    name: String,
    /// The name as a statement writes it.
    q: String,
    ty: String,
    collation: String,
    /// Everything after the type and the collation: nullability, default, generation, comment, …
    rest: String,
}

impl Column {
    /// What is compared: the whole definition but the name.
    fn def(&self) -> String {
        let mut out = self.ty.clone();
        if !self.collation.is_empty() {
            out += &format!(" collate {}", self.collation);
        }
        if !self.rest.is_empty() {
            out += &format!(" {}", self.rest);
        }
        out
    }

    /// The definition as a statement writes it, its collation left out where it is the table's.
    /// How it is generated: `stored`, `virtual`, or neither.
    fn generation(&self) -> Option<&'static str> {
        let rest = self.rest.to_ascii_lowercase();
        let after = &rest[rest.find("generated always as")?..];
        if after.contains(" virtual") {
            Some("virtual")
        } else {
            Some("stored")
        }
    }

    /// Its attributes: each word of `rest` outside parentheses (an expression's) and strings (a
    /// comment's), uppercased, with its byte range.
    fn attributes(&self) -> Vec<(String, usize, usize)> {
        let mut depth = 0usize;
        let mut out = Vec::new();
        for t in tokenize(&self.rest, Dialect::Mysql) {
            match (t.kind, t.text.as_str()) {
                (TokenKind::Symbol, "(") => depth += 1,
                (TokenKind::Symbol, ")") => depth = depth.saturating_sub(1),
                (TokenKind::Word, _) if depth == 0 => out.push((t.upper(), t.start, t.end)),
                _ => {}
            }
        }
        out
    }

    fn not_null(&self) -> bool {
        self.attributes().windows(2).any(|w| w[0].0 == "NOT" && w[1].0 == "NULL")
    }

    fn counts(&self) -> bool {
        self.attributes().iter().any(|(w, _, _)| w == "AUTO_INCREMENT")
    }

    /// The column made NOT NULL, without AUTO_INCREMENT.
    fn held(&self) -> Column {
        let attributes = self.attributes();
        let mut cut = Vec::new();
        let mut k = 0;
        while k < attributes.len() {
            let (w, start, end) = &attributes[k];
            let next = attributes.get(k + 1).filter(|(n, _, _)| w == "NOT" && n == "NULL");
            let before = k.checked_sub(1).map(|p| attributes[p].0.as_str());
            if let Some((_, _, next_end)) = next {
                cut.push((*start, *next_end));
                k += 1;
            } else if w == "AUTO_INCREMENT" || (w == "NULL" && before != Some("DEFAULT")) {
                cut.push((*start, *end));
            }
            k += 1;
        }
        let mut rest = self.rest.clone();
        for (start, end) in cut.into_iter().rev() {
            let end = end + rest[end..].len() - rest[end..].trim_start().len();
            rest.replace_range(start..end, "");
        }
        // Last, which MySQL takes after any attribute, a generation's included, where it takes none before one.
        Column { rest: format!("{} not null", rest.trim()).trim_start().to_string(), ..self.clone() }
    }

    fn written(&self, table_collation: Option<&str>) -> String {
        let mut c = self.clone();
        if table_collation == Some(c.collation.as_str()) {
            c.collation.clear();
        }
        // MySQL shows a nullable AUTO_INCREMENT column without its NULL, and makes one written so
        // NOT NULL.
        if c.counts() && !c.not_null() && !c.attributes().iter().any(|(w, _, _)| w == "NULL") {
            c.rest = format!("null {}", c.rest);
        }
        format!("{} {}", self.q, c.def())
    }
}

/// A table's body: what `create table` makes and `alter table` changes.
#[derive(Clone, Debug, Default)]
struct Body {
    q: String,
    /// The table options, by name (`engine`, `collate`, `comment`, `row_format`, …), as the server wrote them.
    options: Vec<(String, String)>,
    partition: String,
    columns: Vec<Column>,
    /// Indexes and checks, each `(name, name as written, definition)`.
    indexes: Vec<(String, String, String)>,
    checks: Vec<(String, String, String)>,
    /// Its next AUTO_INCREMENT value: no part of its definition, which follows the rows, but the
    /// start a new table is made with.
    counter: Option<String>,
}

impl Body {
    fn to_json(&self) -> String {
        json!({
            "q": self.q,
            "options": self.options,
            "partition": self.partition,
            "columns": self.columns.iter().map(|c| json!([c.name, c.q, c.ty, c.collation, c.rest])).collect::<Vec<_>>(),
            "indexes": self.indexes,
            "checks": self.checks,
        })
        .to_string()
    }

    fn from_json(text: &str) -> Option<Body> {
        let v: Value = serde_json::from_str(text).ok()?;
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let triples = |v: &Value| -> Vec<(String, String, String)> { v.as_array().into_iter().flatten().map(|t| (s(&t[0]), s(&t[1]), s(&t[2]))).collect() };
        Some(Body {
            q: s(&v["q"]),
            options: v["options"].as_array().into_iter().flatten().map(|p| (s(&p[0]), s(&p[1]))).collect(),
            partition: s(&v["partition"]),
            columns: v["columns"].as_array().into_iter().flatten().map(|c| Column { name: s(&c[0]), q: s(&c[1]), ty: s(&c[2]), collation: s(&c[3]), rest: s(&c[4]) }).collect(),
            indexes: triples(&v["indexes"]),
            checks: triples(&v["checks"]),
            counter: None,
        })
    }

    fn option(&self, key: &str) -> Option<&str> {
        self.options.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// The schema of the database `db` is connected to. Under ANSI_QUOTES the server quotes names with
/// double quotes in what it renders, so the session reads without it while it reads.
pub async fn read(db: &mut Db) -> Result<Schema, Error> {
    let mode = db.rows("select @@session.sql_mode as m").await?.first().map(|r| r.text("m").to_string()).unwrap_or_default();
    let plain: Vec<&str> = mode.split(',').filter(|m| !matches!(*m, "ANSI_QUOTES" | "ANSI")).collect();
    let changed = plain.len() != mode.split(',').filter(|m| !m.is_empty()).count();
    if changed {
        db.exec(&format!("set session sql_mode = {}", literal(&plain.join(",")))).await?;
    }
    let result = read_objects(db).await;
    if changed {
        db.exec(&format!("set session sql_mode = {}", literal(&mode))).await?;
    }
    result
}

async fn read_objects(db: &mut Db) -> Result<Schema, Error> {
    let kept = not_history("table_name");
    let kept_trigger = not_history("event_object_table");
    let first = |rows: Vec<Row>, name: &str| rows.first().map(|r| r.text(name).to_string()).unwrap_or_default();
    let ctx = Ctx {
        db: first(db.rows("select database() as n").await?, "n"),
        reserved: db.rows("select word as word from information_schema.keywords where reserved = 1").await?.iter().map(|r| r.text("word").to_ascii_uppercase()).collect(),
        user: first(db.rows("select current_user() as u").await?, "u"),
        engine: first(db.rows("select @@default_storage_engine as e").await?, "e"),
        collation: first(db.rows("select @@collation_database as c").await?, "c"),
    };
    let mut s = Schema::default();

    let mut infos: BTreeMap<(String, String), Row> = BTreeMap::new();
    for r in db
        .rows(&format!(
            "select table_name as t, column_name as c, column_type as type, collation_name as collation, generation_expression as gen, extra as extra,
              case when extra like '%DEFAULT_GENERATED%' then column_default end as expression
            from information_schema.columns where table_schema = database() and {kept}"
        ))
        .await?
    {
        infos.insert((r.text("t").to_string(), r.text("c").to_string()), r);
    }
    let mut checks: BTreeMap<(String, String), String> = BTreeMap::new();
    for r in db
        .rows(
            "select tc.table_name as t, tc.constraint_name as n, cc.check_clause as clause, tc.enforced as enforced
            from information_schema.table_constraints tc join information_schema.check_constraints cc
              on cc.constraint_schema = tc.constraint_schema and cc.constraint_name = tc.constraint_name
            where tc.table_schema = database() and tc.constraint_type = 'CHECK'",
        )
        .await?
    {
        let clause = format!("constraint {} check ({}){}", backtick(r.text("n")), stored_text(r.text("clause")), if r.text("enforced") == "NO" { " not enforced" } else { "" });
        checks.insert((r.text("t").to_string(), r.text("n").to_string()), clause);
    }

    // Each index's parts, to know the one a foreign key made for itself.
    let mut parts: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut unique: BTreeSet<(String, String)> = BTreeSet::new();
    for r in db
        .rows(&format!(
            "select table_name as t, index_name as i, non_unique as nonunique, column_name as col, sub_part as sub, expression as expr
            from information_schema.statistics where table_schema = database() and {kept} order by table_name, index_name, seq_in_index"
        ))
        .await?
    {
        let key = (r.text("t").to_string(), r.text("i").to_string());
        if r.text("nonunique") == "0" {
            unique.insert(key.clone());
        }
        let part = if r.get("expr").is_some() || r.get("sub").is_some() { String::new() } else { r.text("col").to_string() };
        parts.entry(key).or_default().push(part);
    }

    // Foreign keys: their columns, the columns they reference, and their types.
    struct Fk {
        cols: Vec<String>,
        ref_schema: String,
        ref_table: String,
        refs: Vec<String>,
    }
    let mut fks: BTreeMap<(String, String), Fk> = BTreeMap::new();
    for r in db
        .rows(&format!(
            "select table_name as t, constraint_name as n, column_name as col, referenced_table_schema as rs, referenced_table_name as rt, referenced_column_name as rc
            from information_schema.key_column_usage where table_schema = database() and referenced_table_name is not null and {kept}
            order by table_name, constraint_name, ordinal_position"
        ))
        .await?
    {
        let fk = fks.entry((r.text("t").to_string(), r.text("n").to_string())).or_insert_with(|| Fk { cols: vec![], ref_schema: r.text("rs").into(), ref_table: r.text("rt").into(), refs: vec![] });
        fk.cols.push(r.text("col").into());
        fk.refs.push(r.text("rc").into());
    }
    // A key between tables that reach each other through keys: neither table can be dropped while
    // the other's key stands, so such a key is dropped by a statement of its own, before either,
    // where every other key goes with its table.
    let mut refers: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for ((t, _), fk) in &fks {
        if fk.ref_schema == ctx.db && fk.ref_table != *t {
            refers.entry(t.as_str()).or_default().insert(fk.ref_table.as_str());
        }
    }
    let reaches = |from: &str, to: &str| -> bool {
        let mut seen = BTreeSet::new();
        let mut queue = vec![from];
        while let Some(at) = queue.pop() {
            if at == to {
                return true;
            }
            if seen.insert(at) {
                queue.extend(refers.get(at).into_iter().flatten().copied());
            }
        }
        false
    };
    let in_cycle: BTreeSet<(String, String)> = fks.iter().filter(|((t, _), fk)| fk.ref_schema == ctx.db && fk.ref_table != *t && reaches(&fk.ref_table, t)).map(|(k, _)| k.clone()).collect();

    let tables = db.rows(&format!("select table_name as name, table_type as type, table_collation as collation from information_schema.tables where table_schema = database() and {kept} order by table_name")).await?;
    let views: BTreeSet<String> = tables.iter().filter(|r| r.text("type") == "VIEW").map(|r| r.text("name").to_string()).collect();
    for r in tables.iter().filter(|r| r.text("type") != "VIEW") {
        let name = r.text("name").to_string();
        let q = ctx.ident(&name);
        let text = first(db.rows(&format!("show create table {}", backtick(&name))).await?, "Create Table");
        let mut body = parse_table(&ctx, &text, &name, &infos, &checks);
        body.q = q.clone();
        // A table's default collation, which `show create table` leaves out where it is its
        // character set's own, and which says the character set too.
        if let Some(collation) = r.get("collation") {
            body.options.retain(|(k, _)| k != "charset" && k != "collate");
            body.options.push(("collate".into(), collation.to_string()));
            body.options.sort();
        }
        // A foreign key's own index: named as the key, on exactly its columns, which adding the key
        // makes where no index serves it. It stays in the table's body as any index does, so that
        // the body's change swaps the index that serves a key for another in one statement, which
        // MySQL takes where it refuses either alone; a new table, or a new key, leaves it to the key.
        let keys: Vec<String> = fks.keys().filter(|(t, _)| *t == name).map(|(_, n)| n.clone()).collect();
        let owned: Vec<String> = fks.iter().filter(|((t, n), fk)| *t == name && parts.get(&(t.clone(), n.clone())).is_some_and(|p| *p == fk.cols) && !unique.contains(&(t.clone(), n.clone())) && n != "PRIMARY").map(|((_, n), _)| n.clone()).collect();
        let key_lines = vec![format!("{KEYS}{}", json!(keys)), format!("{OWN}{}", json!(owned))];
        let mut made = body.clone();
        made.indexes.retain(|(i, _, _)| !owned.contains(i));
        let id = format!("table {name}");
        let def = body.to_json();
        // Every column is an object of its own, which carries no statement: the table's own
        // statement makes and changes it. Each carries its table's body, which a rename writes.
        for c in &body.columns {
            let mut deps = BTreeSet::new();
            if let Some(info) = infos.get(&(name.clone(), c.name.clone())) {
                for t in tokenize(info.text("gen"), Dialect::Mysql) {
                    if let Some(other) = name_of(&t).filter(|_| t.kind == TokenKind::Quoted) {
                        if other != c.name && body.column(&other).is_some() {
                            deps.insert(format!("column {name}.{other}"));
                        }
                    }
                }
            }
            s.add(Object {
                id: format!("column {name}.{}", c.name),
                kind: Some(Kind::Column),
                owner: Some(id.clone()),
                def: c.def(),
                inline: true,
                deps,
                change: Change::Alter { blocks: false },
                extra: [vec![def.clone()], key_lines.clone()].concat(),
                ..Object::default()
            });
        }
        s.add(Object { id: id.clone(), kind: Some(Kind::Table), create: vec![create_table(&ctx, &made)], drop: vec![format!("drop table {q}")], def, change: Change::Alter { blocks: false }, extra: key_lines, ..Object::default() });

        // Foreign keys, from the server's rendering of each.
        for element in elements(&text) {
            let tokens = tokenize(&element, Dialect::Mysql);
            if !(tokens.first().is_some_and(|t| t.upper() == "CONSTRAINT") && tokens.iter().any(|t| t.upper() == "FOREIGN")) {
                continue;
            }
            let Some(fk_name) = tokens.get(1).and_then(name_of) else { continue };
            let Some(fk) = fks.get(&(name.clone(), fk_name.clone())) else { continue };
            let clause = ctx.tidy(&element, TABLE_TEXT);
            let same_db = fk.ref_schema == ctx.db;
            // What MySQL won't change on either side of a key while it stands is part of it: each
            // column's type, collation and AUTO_INCREMENT, so that the key is dropped before such a
            // change and added after.
            let held = |t: &str, c: &str| {
                let Some(r) = infos.get(&(t.to_string(), c.to_string())) else { return String::new() };
                let collation = r.get("collation").map(|n| format!(" collate {n}")).unwrap_or_default();
                let counts = if r.text("extra").contains("auto_increment") { " auto_increment" } else { "" };
                format!("{}{collation}{counts}", r.text("type"))
            };
            let columns = fk.cols.iter().map(|c| held(&name, c)).chain(fk.refs.iter().map(|c| if same_db { held(&fk.ref_table, c) } else { String::new() })).collect::<Vec<_>>().join(", ");
            let mut deps: BTreeSet<String> = fk.cols.iter().map(|c| format!("column {name}.{c}")).collect();
            let cycle = in_cycle.contains(&(name.clone(), fk_name.clone()));
            if cycle {
                deps.insert(id.clone());
            }
            if same_db {
                deps.insert(format!("table {}", fk.ref_table));
                deps.extend(fk.refs.iter().map(|c| format!("column {}.{c}", fk.ref_table)));
            }
            let fq = ctx.ident(&fk_name);
            // MySQL renames an index it made for a key to the name of the next key that uses it:
            // renamed to itself as the key goes, it is the table's own, and keeps its name when a
            // key is added again (a key's actions change by dropping it and adding it).
            let mut drop = format!("alter table {q} drop foreign key {fq}");
            let serving = parts.iter().filter(|((t, i), p)| *t == name && i != "PRIMARY" && *i != fk_name && p.len() >= fk.cols.len() && p[..fk.cols.len()] == fk.cols[..]);
            for ((_, i), _) in serving {
                let iq = ctx.ident(i);
                drop += &format!(", rename index {iq} to {iq}");
            }
            s.add(Object {
                id: format!("foreign key {name}.{fk_name}"),
                kind: Some(Kind::Constraint),
                owner: (!cycle).then(|| id.clone()),
                def: format!("{clause} ({columns})"),
                create: vec![format!("alter table {q} add {clause}")],
                drop: vec![drop],
                deps,
                extra: tokenize(&clause, Dialect::Mysql)
                    .windows(2)
                    .any(|w| w[0].upper() == "SET" && w[1].upper() == "NULL")
                    .then(|| format!("{SETS_NULL}{}", json!(fk.cols.iter().map(|c| format!("column {name}.{c}")).collect::<Vec<_>>())))
                    .into_iter()
                    .collect(),
                ..Object::default()
            });
        }
    }

    // Views, from the server's rendering, and what each reads.
    let mut reads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in db.rows("select view_name as v, table_schema as s, table_name as t from information_schema.view_table_usage where view_schema = database()").await? {
        if r.text("s") == ctx.db {
            let t = r.text("t");
            reads.entry(r.text("v").to_string()).or_default().insert(if views.contains(t) { format!("view {t}") } else { format!("table {t}") });
        }
    }
    for r in db.rows("select table_name as v, specific_schema as s, specific_name as f from information_schema.view_routine_usage where table_schema = database()").await? {
        if r.text("s") == ctx.db {
            reads.entry(r.text("v").to_string()).or_default().insert(format!("function {}", r.text("f")));
        }
    }
    for name in &views {
        let text = first(db.rows(&format!("show create view {}", backtick(name))).await?, "Create View");
        let Some(view) = parse_view(&ctx, &text) else { continue };
        let q = ctx.ident(name);
        let create = format!("create {}view {q}{} as {}{}", view.head, view.columns, view.body, view.check);
        s.add(Object {
            id: format!("view {name}"),
            kind: Some(Kind::View),
            def: format!("{}{}{} as {}", view.def_head, view.columns, view.check, view.body),
            create: vec![create],
            drop: vec![format!("drop view {q}")],
            deps: reads.remove(name).unwrap_or_default().into_iter().filter(|d| *d != format!("view {name}")).collect(),
            change: Change::Alter { blocks: false },
            ..Object::default()
        });
    }

    // Routines: functions and procedures, each in a namespace of its own.
    for r in db.rows("select routine_name as n, routine_type as type, routine_definition as body, definer as definer from information_schema.routines where routine_schema = database() order by routine_name").await? {
        let (name, word) = (r.text("n"), r.text("type").to_ascii_lowercase());
        let q = ctx.ident(name);
        let column = if word == "function" { "Create Function" } else { "Create Procedure" };
        let text = first(db.rows(&format!("show create {word} {}", backtick(name))).await?, column);
        let (head, body) = split_body(&ctx, &text, r.text("body"));
        let head = head.replacen("create ", &format!("create {}", ctx.definer(r.text("definer"))), 1);
        // A body of several lines starts on a line of its own.
        let create = format!("{head}{}{body}", if body.contains('\n') { "\n" } else { " " });
        s.add(Object {
            id: format!("{word} {name}"),
            kind: Some(Kind::Function),
            def: format!("{head}\n{body}\n-- {}", ctx.definer(r.text("definer"))),
            create: vec![create],
            drop: vec![format!("drop {word} {q}")],
            change: Change::Alter { blocks: false },
            ..Object::default()
        });
    }

    // Triggers, in their order on their table: each after the one before it, which it follows.
    let triggers = db
        .rows(&format!(
            "select trigger_name as n, event_manipulation as ev, event_object_table as t, action_statement as body, action_timing as timing, action_order as ord, definer as definer
            from information_schema.triggers where trigger_schema = database() and {kept_trigger} order by event_object_table, action_timing, event_manipulation, action_order"
        ))
        .await?;
    for (k, r) in triggers.iter().enumerate() {
        let name = r.text("n");
        let q = ctx.ident(name);
        let before = k.checked_sub(1).map(|j| &triggers[j]).filter(|p| p.text("t") == r.text("t") && p.text("timing") == r.text("timing") && p.text("ev") == r.text("ev"));
        let follows = before.map(|p| format!("follows {} ", ctx.ident(p.text("n")))).unwrap_or_default();
        let body = ctx.tidy(r.text("body"), BODY_TEXT);
        let head = format!("{} {} on {} for each row {follows}", r.text("timing").to_ascii_lowercase(), r.text("ev").to_ascii_lowercase(), ctx.ident(r.text("t")));
        s.add(Object {
            id: format!("trigger {name}"),
            kind: Some(Kind::Trigger),
            owner: Some(format!("table {}", r.text("t"))),
            def: format!("{head}{body}\n-- {}", ctx.definer(r.text("definer"))),
            create: vec![format!("create {}trigger {q} {head}{body}", ctx.definer(r.text("definer")))],
            drop: vec![format!("drop trigger {q}")],
            deps: before.map(|p| format!("trigger {}", p.text("n"))).into_iter().collect(),
            ..Object::default()
        });
    }

    // Events. When one starts is a time the server fixes as the event is made, so two databases
    // made apart differ there however they were written: it is not compared, and it is written only
    // where the schema named a time of its own, not the moment the event was made. Nor is when a
    // one-time event runs, for the same reason.
    for r in db
        .rows(
            "select event_name as n, definer as definer, event_definition as body, event_type as type, interval_value as iv, interval_field as field, ends as ends, status as status, on_completion as oc, event_comment as comment,
              starts = created as now
            from information_schema.events where event_schema = database() order by event_name",
        )
        .await?
    {
        let name = r.text("n");
        let q = ctx.ident(name);
        let text = first(db.rows(&format!("show create event {}", backtick(name))).await?, "Create Event");
        let (head, body) = split_body(&ctx, &text, r.text("body"));
        let head = without_defaults(&head, r.flag("now"));
        let def = format!(
            "{} every {} {} ends {} {} {} comment {}\n{body}\n-- {}",
            r.text("type"),
            r.text("iv"),
            r.text("field"),
            r.text("ends"),
            r.text("status"),
            r.text("oc"),
            r.text("comment"),
            ctx.definer(r.text("definer"))
        );
        s.add(Object {
            id: format!("event {name}"),
            kind: Some(Kind::Trigger),
            def,
            create: vec![format!("{} {body}", head.replacen("create ", &format!("create {}", ctx.definer(r.text("definer"))), 1))],
            drop: vec![format!("drop event {q}")],
            change: Change::Alter { blocks: false },
            ..Object::default()
        });
    }

    // Privileges on the tables, views, columns and routines. MySQL keeps one after what it is on is
    // dropped or renamed, so it is no part of that object and goes with nothing: it is revoked
    // before a drop and granted after a create.
    let grants = crate::catalog::mysql_grants(db).await?;
    s.grants_unread = grants.is_none();
    for g in grants.unwrap_or_default() {
        let on = match g.kind {
            "table" if views.contains(&g.name) => format!("view {}", g.name),
            "table" => format!("table {}", g.name),
            kind => format!("{kind} {}", g.name),
        };
        let at = g.column.as_ref().map(|c| format!("column {}.{c}", g.name)).unwrap_or_else(|| on.clone());
        let column = g.column.as_ref().map(|c| format!(" ({c})")).unwrap_or_default();
        let ident = |name: &str| ctx.ident(name);
        s.add(Object {
            // By what it is on and to whom first, so that a plan writes those of one statement together.
            id: format!("grant on {on} to {}: {}{column}", g.account(), g.privilege),
            kind: Some(Kind::Grant),
            create: vec![g.statement(&ident, true)],
            drop: vec![g.statement(&ident, false)],
            deps: s.objects.contains_key(&at).then_some(at).into_iter().collect(),
            ..Object::default()
        });
    }
    Ok(s)
}

/// An event's head with what it would be anyway left out: `on completion not preserve`, `enable`,
/// and, where `now` says the server chose it, when it starts.
fn without_defaults(head: &str, now: bool) -> String {
    let tokens = tokenize(head, Dialect::Mysql);
    let words: Vec<String> = tokens.iter().map(Token::upper).collect();
    let mut ranges = Vec::new();
    let mut k = 0;
    while k < tokens.len() {
        let at = |w: &[&str]| words.len() >= k + w.len() && words[k..k + w.len()].iter().zip(w).all(|(a, b)| a == b);
        let width = if at(&["ON", "COMPLETION", "NOT", "PRESERVE"]) {
            4
        } else if at(&["ENABLE"]) {
            1
        } else if now && at(&["STARTS"]) && tokens.get(k + 1).is_some_and(|t| t.kind == TokenKind::Str) {
            2
        } else {
            0
        };
        if width > 0 {
            ranges.push((tokens[k].start, tokens[k + width - 1].end));
            k += width;
        } else {
            k += 1;
        }
    }
    cut(head, &ranges)
}

/// The elements of a `create table` the server rendered: each column, index and constraint, as
/// written between its parentheses.
fn elements(text: &str) -> Vec<String> {
    let text = open_version_comments(text);
    let tokens = tokenize(&text, Dialect::Mysql);
    let Some(open) = tokens.iter().position(|t| t.text == "(") else { return vec![] };
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for t in &tokens[open..] {
        match t.text.as_str() {
            "(" => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            ")" => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        out.push(text[s..t.start].trim().to_string());
                    }
                    break;
                }
            }
            "," if depth == 1 => {
                if let Some(s) = start.take() {
                    out.push(text[s..t.start].trim().to_string());
                }
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(t.start);
        }
    }
    out
}

/// An expression as information_schema stores it (a check's clause, a generated column's
/// expression): escaped as a string's contents, and its bytes of UTF-8 read one character each.
/// `show create table` renders the same expressions with every character beyond three bytes of
/// UTF-8 replaced by `?`, so these are read from here and decoded.
pub(crate) fn stored_text(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                unescaped.push(next);
            }
            continue;
        }
        unescaped.push(c);
    }
    if unescaped.chars().all(|c| (c as u32) < 0x100) && unescaped.chars().any(|c| (c as u32) >= 0x80) {
        if let Ok(decoded) = String::from_utf8(unescaped.chars().map(|c| c as u8).collect()) {
            return decoded;
        }
    }
    unescaped
}

/// A column's attributes with its generation expression put back as `expression` says.
fn generated_from(attributes: &str, expression: &str) -> String {
    let tokens = tokenize(attributes, Dialect::Mysql);
    let Some(k) = tokens.windows(2).position(|w| w[0].upper() == "AS" && w[1].text == "(") else { return attributes.to_string() };
    let mut depth = 0;
    for t in &tokens[k + 1..] {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return format!("{}({expression}){}", &attributes[..tokens[k + 1].start], &attributes[t.end..]);
                }
            }
            _ => {}
        }
    }
    attributes.to_string()
}

/// A column's attributes with its default's parenthesized expression replaced by `expression`.
fn default_from(attributes: &str, expression: &str) -> String {
    let tokens = tokenize(attributes, Dialect::Mysql);
    let Some(k) = tokens.windows(2).position(|w| w[0].upper() == "DEFAULT" && w[1].text == "(") else { return attributes.to_string() };
    let mut depth = 0;
    for t in &tokens[k + 1..] {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return format!("{}({expression}){}", &attributes[..tokens[k + 1].start], &attributes[t.end..]);
                }
            }
            _ => {}
        }
    }
    attributes.to_string()
}

/// What follows the elements of a `create table` the server rendered: its options and partitioning.
fn after_elements(text: &str) -> String {
    let text = open_version_comments(text);
    let mut depth = 0;
    for t in tokenize(&text, Dialect::Mysql) {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return text[t.end..].to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// The table's body, from `show create table`'s text and the columns' information_schema rows.
fn parse_table(ctx: &Ctx, text: &str, table: &str, infos: &BTreeMap<(String, String), Row>, checks: &BTreeMap<(String, String), String>) -> Body {
    let mut body = Body::default();
    for element in elements(text) {
        let tokens = tokenize(&element, Dialect::Mysql);
        let Some(head) = tokens.first() else { continue };
        if head.kind == TokenKind::Quoted {
            let name = name_of(head).unwrap_or_default();
            let info = infos.get(&(table.to_string(), name.clone()));
            let rest = element[head.end..].trim();
            let ty = info.map(|i| i.text("type").to_string()).unwrap_or_default();
            let after = if !ty.is_empty() && rest.len() >= ty.len() && rest[..ty.len()].eq_ignore_ascii_case(&ty) { &rest[ty.len()..] } else { rest };
            let ty = if after.len() == rest.len() { String::new() } else { ty };
            let after = match info.map(|i| i.text("gen")).filter(|g| !g.is_empty()) {
                Some(gen) => generated_from(after, &stored_text(gen)),
                None => after.to_string(),
            };
            // An expression default as it was written: `show create table` prints a literal
            // converted to the character set of what it meets (`uuid()`'s utf8mb3), which reads
            // back as another expression.
            let after = match info.and_then(|i| i.get("expression")).filter(|e| !e.is_empty()) {
                Some(expression) => default_from(&after, &stored_text(expression)),
                None => after,
            };
            body.columns.push(Column {
                q: ctx.ident(&name),
                name,
                ty: ctx.tidy(&ty, Tidy { collapse: false, ..TABLE_TEXT }),
                collation: info.and_then(|i| i.get("collation")).unwrap_or_default().to_string(),
                rest: column_rest(&ctx.tidy(&after, TABLE_TEXT)),
            });
            continue;
        }
        let tidy = ctx.tidy(&element, TABLE_TEXT);
        match head.upper().as_str() {
            "PRIMARY" => body.indexes.push(("PRIMARY".into(), String::new(), tidy)),
            "CONSTRAINT" => {
                if tokens.iter().any(|t| t.upper() == "FOREIGN") {
                    continue;
                }
                let name = tokens.get(1).and_then(name_of).unwrap_or_default();
                let tidy = checks.get(&(table.to_string(), name.clone())).map(|c| ctx.tidy(c, TABLE_TEXT)).unwrap_or(tidy);
                body.checks.push((name.clone(), ctx.ident(&name), tidy));
            }
            _ => {
                let Some(name) = tokens.iter().find(|t| t.kind == TokenKind::Quoted).and_then(name_of) else { continue };
                body.indexes.push((name.clone(), ctx.ident(&name), tidy));
            }
        }
    }
    // MySQL keeps an expression as it was written, and a column's name in another case is the same
    // column: each is written as the table names it.
    for (_, _, def) in body.indexes.iter_mut().filter(|(_, _, d)| d.contains("((")) {
        *def = columns_as_named(def, &body.columns, &ctx.reserved);
    }
    // In name order, the primary key first, so that two tables of the same indexes have one body
    // whatever order they were made in.
    body.indexes.sort_by(|a, b| (a.0 != "PRIMARY", &a.0).cmp(&(b.0 != "PRIMARY", &b.0)));
    body.checks.sort();
    let close = after_elements(text);
    let tokens = tokenize(&close, Dialect::Mysql);
    let mut k = 0;
    while k < tokens.len() {
        if tokens[k].upper() == "PARTITION" && tokens.get(k + 1).is_some_and(|t| t.upper() == "BY") {
            body.partition = columns_as_named(&ctx.tidy(&close[tokens[k].start..], TABLE_TEXT), &body.columns, &ctx.reserved);
            break;
        }
        // `KEY=VALUE`, where the key may be two words (`DEFAULT CHARSET`) and the value a list.
        let mut key = Vec::new();
        while k < tokens.len() && tokens[k].kind == TokenKind::Word && key.len() < 3 {
            key.push(tokens[k].upper());
            k += 1;
        }
        if tokens.get(k).is_some_and(|t| t.text == "=") {
            k += 1;
        }
        let Some(value) = tokens.get(k) else { break };
        let (start, mut end) = (value.start, value.end);
        if value.text == "(" {
            let mut depth = 0;
            while k < tokens.len() {
                match tokens[k].text.as_str() {
                    "(" => depth += 1,
                    ")" => depth -= 1,
                    _ => {}
                }
                end = tokens[k].end;
                if depth == 0 {
                    break;
                }
                k += 1;
            }
        }
        k += 1;
        let key = key.into_iter().filter(|w| w != "DEFAULT").collect::<Vec<_>>().join(" ").to_ascii_lowercase();
        let key = if key == "character set" { "charset".to_string() } else { key };
        // A table in a file of its own, as one with no tablespace is where `innodb_file_per_table` is
        // on: no statement takes the clause away once written.
        let own_file = key == "tablespace" && close[start..end].trim_matches('`').eq_ignore_ascii_case("innodb_file_per_table");
        if key == "auto_increment" {
            body.counter = Some(close[start..end].to_string());
        } else if !key.is_empty() && !own_file {
            body.options.push((key, close[start..end].to_string()));
        }
    }
    body.options.sort();
    body
}

/// A column's attributes with what the definition carries elsewhere taken out: its character set
/// and collation (the column's collation is always part of the definition, since a table's
/// default can change under it), and `default null`, which a nullable column has unsaid.
fn column_rest(text: &str) -> String {
    let tokens = tokenize(text, Dialect::Mysql);
    let mut ranges = Vec::new();
    let mut k = 0;
    // Only the column's own: a default's or a generated column's expression keeps its casts'.
    let mut depth = 0;
    while k < tokens.len() {
        match tokens[k].text.as_str() {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        let w = tokens[k].upper();
        let width = match (w.as_str(), tokens.get(k + 1).map(Token::upper).as_deref()) {
            _ if depth > 0 => 0,
            ("CHARACTER", Some("SET")) => 3,
            ("CHARSET" | "COLLATE", _) => 2,
            ("DEFAULT", Some("NULL")) => 2,
            _ => 0,
        };
        if width > 0 && k + width <= tokens.len() {
            ranges.push((tokens[k].start, tokens[k + width - 1].end));
            k += width;
        } else {
            k += 1;
        }
    }
    cut(text, &ranges)
}

/// `create table` for the body, with what the database would choose anyway left unsaid.
fn create_table(ctx: &Ctx, body: &Body) -> String {
    let collation = body.option("collate").unwrap_or_default();
    let mut lines: Vec<String> = body.columns.iter().map(|c| c.written(Some(collation))).collect();
    lines.extend(body.indexes.iter().map(|(_, _, d)| d.clone()));
    lines.extend(body.checks.iter().map(|(_, _, d)| d.clone()));
    let mut out = format!("create table {} (\n  {}\n)", body.q, lines.join(",\n  "));
    for (key, value) in &body.options {
        let skip = match key.as_str() {
            "engine" => value.eq_ignore_ascii_case(&ctx.engine),
            "charset" => true,
            "collate" => *value == ctx.collation,
            _ => false,
        };
        if !skip {
            out += &format!(" {key}={value}");
        }
    }
    if let Some(counter) = &body.counter {
        out += &format!(" auto_increment={counter}");
    }
    if !body.partition.is_empty() {
        out += &format!("\n{}", body.partition);
    }
    out
}

/// A view as the server rendered it, in parts.
struct View {
    /// What `create` writes before `view`: the algorithm, definer and security that are not the defaults.
    head: String,
    /// The same, every part said, for the comparison.
    def_head: String,
    columns: String,
    body: String,
    check: String,
}

fn parse_view(ctx: &Ctx, text: &str) -> Option<View> {
    let tokens = tokenize(text, Dialect::Mysql);
    let at = tokens.iter().position(|t| t.upper() == "VIEW")?;
    let word_after = |w: &str| tokens[..at].iter().position(|t| t.upper() == w).and_then(|k| tokens[k + 1..at].iter().find(|t| t.text != "=")).map(|t| t.text.to_ascii_uppercase()).unwrap_or_default();
    let algorithm = word_after("ALGORITHM");
    let security = word_after("SECURITY");
    let definer = tokens[..at].iter().position(|t| t.upper() == "DEFINER").map(|k| tokens[k + 1..at].iter().skip_while(|t| t.text == "=").take_while(|t| t.upper() != "SQL").filter_map(|t| name_of(t).or_else(|| (t.text == "@").then(|| "@".to_string()))).collect::<String>()).unwrap_or_default();
    let mut k = at + 2;
    let mut columns = String::new();
    if tokens.get(k).is_some_and(|t| t.text == "(") {
        let end = tokens[k..].iter().position(|t| t.text == ")")? + k;
        columns = format!(" {}", ctx.tidy(&text[tokens[k].start..tokens[end].end], SERVER_TEXT));
        k = end + 1;
    }
    if tokens.get(k).map(Token::upper).as_deref() != Some("AS") {
        return None;
    }
    let mut end = text.len();
    let mut check = String::new();
    let n = tokens.len();
    if n >= 4 && tokens[n - 4].upper() == "WITH" && tokens[n - 2].upper() == "CHECK" && tokens[n - 1].upper() == "OPTION" {
        end = tokens[n - 4].start;
        check = format!(" with {} check option", tokens[n - 3].text.to_ascii_lowercase());
    }
    let body = ctx.tidy(&text[tokens[k].end..end], SERVER_TEXT);
    let mut head = String::new();
    if algorithm != "UNDEFINED" && !algorithm.is_empty() {
        head += &format!("algorithm = {} ", algorithm.to_ascii_lowercase());
    }
    head += &ctx.definer(&definer);
    if security == "INVOKER" {
        head += "sql security invoker ";
    }
    Some(View { def_head: format!("algorithm={algorithm} definer={definer} security={security}"), head, columns, body, check })
}

/// A routine's or an event's `create` as the server rendered it, split into its head (tidied, its
/// definer left out) and its body (as written, the database's own name taken out).
fn split_body(ctx: &Ctx, text: &str, body: &str) -> (String, String) {
    let (head, body) = match text.strip_suffix(body).filter(|_| !body.is_empty()) {
        Some(head) => (head, body),
        None => (text, ""),
    };
    let tokens = tokenize(head, Dialect::Mysql);
    let mut ranges = Vec::new();
    // DEFINER = `user`@`host`
    if let Some(k) = tokens.iter().position(|t| t.upper() == "DEFINER") {
        let mut j = k + 1;
        if tokens.get(j).is_some_and(|t| t.text == "=") {
            j += 1;
        }
        if tokens.get(j + 1).is_some_and(|t| t.text == "@") {
            j += 2;
        }
        if let Some(end) = tokens.get(j) {
            ranges.push((tokens[k].start, end.end));
        }
    }
    let head = cut(head, &ranges);
    // The routine's own name is the one name before a parenthesis in its head.
    (ctx.tidy(&head, TABLE_TEXT), ctx.tidy(body, BODY_TEXT).trim().to_string())
}

/// The columns renamed between two bodies, `(old, new)`, by the planner's rule: a column that
/// goes while one of the same definition comes, alone in its table with that definition.
fn renames(from: &Body, to: &Body) -> Vec<(String, String)> {
    let gone: Vec<&Column> = from.columns.iter().filter(|c| to.column(&c.name).is_none()).collect();
    let come: Vec<&Column> = to.columns.iter().filter(|c| from.column(&c.name).is_none()).collect();
    let mut out = Vec::new();
    for g in &gone {
        let same: Vec<&&Column> = come.iter().filter(|c| c.def() == g.def()).collect();
        let rivals = gone.iter().filter(|c| c.def() == g.def()).count();
        if same.len() == 1 && rivals == 1 {
            out.push((g.name.clone(), same[0].name.clone()));
        }
    }
    out
}

/// `text` with the names `map` renames, outside its first parenthesis' head: an index's key parts.
fn renamed_parts(text: &str, map: &BTreeMap<String, String>) -> String {
    let tokens = tokenize(text, Dialect::Mysql);
    let Some(open) = tokens.iter().position(|t| t.text == "(") else { return text.to_string() };
    let mut out = String::new();
    let mut at = 0;
    for t in &tokens[open..] {
        if let Some(new) = name_of(t).and_then(|n| map.get(&n)) {
            out.push_str(&text[at..t.start]);
            out.push_str(new);
            at = t.end;
        }
    }
    out.push_str(&text[at..]);
    out
}

/// The statements that take a table from one body to the other: one `alter table` with a clause
/// per change, and the change of its partitioning, which MySQL takes only as a statement of its own.
/// An index's definition without its block size and full-text parser.
fn without_index_options(def: &str) -> String {
    static OPTIONS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i) (key_block_size\s*=\s*\d+|with parser `[^`]+`|with parser \w+)").expect("a valid pattern"));
    OPTIONS.replace_all(def, "").into_owned()
}

/// MySQL gives each NULL in a column made AUTO_INCREMENT the next value of its sequence, even in
/// strict mode, which refuses a NULL in a column made NOT NULL: so a nullable column made
/// AUTO_INCREMENT NOT NULL is made NOT NULL by a statement before, as it was otherwise (its name, its
/// type, what indexes it), which fails on a NULL rather than numbering it (a row whose key had no
/// parent would take one). Each such column, by the name it had, and its clause.
fn held_columns(from: &Body, to: &Body, pairs: &[(String, String)]) -> Vec<(String, String)> {
    let old_of: BTreeMap<&str, &str> = pairs.iter().map(|(o, n)| (n.as_str(), o.as_str())).collect();
    let collation = (from.option("collate") == to.option("collate")).then(|| to.option("collate").unwrap_or_default());
    let computed = |c: &Column| c.generation() == Some("virtual");
    to.columns
        .iter()
        .filter_map(|c| {
            let was = from.column(old_of.get(c.name.as_str()).copied().unwrap_or(&c.name))?;
            (c.not_null() && c.counts() && !was.counts() && !was.not_null() && !computed(c) && !computed(was)).then(|| (was.name.clone(), format!("modify column {}", was.held().written(collation))))
        })
        .collect()
}

fn alter_table(from: &Body, to: &Body, pairs: &[(String, String)], made: &BTreeSet<String>, late: &BTreeSet<String>) -> Vec<String> {
    let pairs = pairs.to_vec();
    let map: BTreeMap<String, String> = pairs.iter().map(|(old, new)| (old.clone(), to.column(new).map(|c| c.q.clone()).unwrap_or_default())).collect();
    let old_of: BTreeMap<&str, &str> = pairs.iter().map(|(o, n)| (n.as_str(), o.as_str())).collect();
    let mut clauses = Vec::new();

    // A column made or unmade virtual, or moved between virtual and stored, which MySQL won't
    // modify, is dropped and added again in the statement: a virtual column's values are computed,
    // and a column made virtual has its values computed from then on. A generated column reading
    // it goes and comes back after it, since one can read only those defined before it, and so do
    // the checks and indexes reading either, which hold the drop back.
    let remade = |was: &Column, now: &Column| was.generation() != now.generation() && (was.generation() == Some("virtual") || now.generation() == Some("virtual"));
    let kept = |c: &Column| from.column(&c.name).filter(|_| !old_of.contains_key(c.name.as_str()));
    let mut remade_columns: BTreeSet<String> = to.columns.iter().filter(|c| kept(c).is_some_and(|was| remade(was, c))).map(|c| c.name.clone()).collect();
    loop {
        let more: Vec<String> = to.columns.iter().filter(|c| !remade_columns.contains(&c.name) && c.generation().is_some() && kept(c).is_some() && remade_columns.iter().any(|r| reads(&c.rest, r))).map(|c| c.name.clone()).collect();
        if more.is_empty() {
            break;
        }
        remade_columns.extend(more);
    }
    // A column whose spatial reference changes, which MySQL won't change under a spatial index.
    let srid = |c: &Column| c.rest.split_whitespace().skip_while(|w| *w != "srid").nth(1).map(String::from);
    let resrid: BTreeSet<String> = to.columns.iter().filter(|c| kept(c).is_some_and(|was| srid(was) != srid(c))).map(|c| c.name.clone()).collect();
    let checks_on = |def: &str| remade_columns.iter().any(|r| reads(def, r));
    let indexes_on = |def: &str| remade_columns.iter().chain(&resrid).any(|r| reads(def, r));
    // A spatial index goes in a statement before the one that changes its column's reference,
    // which MySQL refuses while the index stands, even in the same statement.
    let mut first: Vec<String> = Vec::new();

    // A check that reads a renamed column is dropped and added again in the same statement, which
    // MySQL requires; an index on one follows the column by itself.
    for (name, q, def) in &from.checks {
        if to.checks.iter().find(|(n, _, _)| n == name).is_none_or(|(_, _, d)| d != def) || checks_on(def) {
            clauses.push(format!("drop check {q}"));
        }
    }
    let plain = |def: &str| !def.contains("((");
    let index_now = |def: &str| if plain(def) { renamed_parts(def, &map) } else { def.to_string() };
    // An index made visible or invisible, and nothing else, changes in place.
    let seen_only = |a: &str, b: &str| a != b && unseen(a).0 == unseen(b).0;
    for (name, q, def) in &from.indexes {
        let now = to.indexes.iter().find(|(n, _, _)| n == name);
        if let Some((_, _, d)) = now.filter(|(_, _, d)| seen_only(&index_now(def), d) && !indexes_on(def)) {
            clauses.push(format!("alter index {q} {}", if d.ends_with(" invisible") { "invisible" } else { "visible" }));
            continue;
        }
        // One whose block size or full-text parser alone changes goes in a statement before:
        // dropped and added again in one, MySQL takes it as the same index and keeps it as it was.
        let options_alone = now.is_some_and(|(_, _, d)| *d != index_now(def) && without_index_options(d) == without_index_options(&index_now(def)));
        if (resrid.iter().any(|r| reads(def, r)) || options_alone) && name != "PRIMARY" {
            first.push(format!("drop index {q}"));
        } else if now.is_none_or(|(_, _, d)| *d != index_now(def)) || indexes_on(def) {
            clauses.push(if name == "PRIMARY" { "drop primary key".to_string() } else { format!("drop index {q}") });
        }
    }
    for c in &from.columns {
        if to.column(&c.name).is_none() && !map.contains_key(&c.name) {
            clauses.push(format!("drop column {}", c.q));
        }
    }
    for c in &to.columns {
        if let Some(was) = kept(c).filter(|_| remade_columns.contains(&c.name)) {
            clauses.push(format!("drop column {}", was.q));
        }
    }
    // A column is written without its collation where it takes the table's default, unless the
    // default changes in this statement, which MySQL applies before the columns.
    let collation = (from.option("collate") == to.option("collate")).then(|| to.option("collate").unwrap_or_default());
    // A renamed column whose definition changes too is renamed and redefined by one `change column`:
    // a later clause of the same statement cannot name it by its new name.
    for (old, new) in &pairs {
        let (was, now) = (from.column(old), to.column(new));
        match (was, now) {
            (Some(was), Some(now)) if was.def() != now.def() => clauses.push(format!("change column {} {}", was.q, now.written(collation))),
            _ => clauses.push(format!("rename column {} to {}", was.map(|c| c.q.as_str()).unwrap_or(old), now.map(|c| c.q.as_str()).unwrap_or(new))),
        }
    }
    for c in &to.columns {
        match from.column(old_of.get(c.name.as_str()).copied().unwrap_or(&c.name)) {
            Some(_) if !old_of.contains_key(c.name.as_str()) && remade_columns.contains(&c.name) => clauses.push(format!("add column {}", c.written(collation))),
            Some(was) if old_of.contains_key(c.name.as_str()) || was.def() == c.def() => {}
            Some(_) => clauses.push(format!("modify column {}", c.written(collation))),
            None => clauses.push(format!("add column {}", c.written(collation))),
        }
    }
    // An index a new key would make is left to it, unless this statement drops one: another key
    // may need it until the new one is in place, so both go in the same statement.
    let swaps = clauses.iter().any(|c| c.starts_with("drop index ") || c == "drop primary key");
    // A column made AUTO_INCREMENT needs a key in the statement that makes it so, which one a
    // foreign key would make only after.
    let counts: Vec<&String> = to.columns.iter().filter(|c| c.counts() && from.column(&c.name).is_none_or(|was| !was.counts())).map(|c| &c.name).collect();
    let keys_a_count = |def: &str| counts.iter().any(|c| reads(def, c));
    // The NOT NULL steps [`held_columns`] writes, each set in a statement of its own: those a key holds
    // back (`late`), which the alterer joins to the key's drop, and the others, which it moves to the
    // start of the plan.
    let (late, early): (Vec<(String, String)>, Vec<(String, String)>) = held_columns(from, to, &pairs).into_iter().partition(|(c, _)| late.contains(c));
    let (late, early): (Vec<String>, Vec<String>) = (late.into_iter().map(|(_, clause)| clause).collect(), early.into_iter().map(|(_, clause)| clause).collect());
    for (name, _, def) in to.indexes.iter().filter(|(n, _, d)| swaps || !made.contains(n) || keys_a_count(d)) {
        let was = from.indexes.iter().find(|(n, _, _)| n == name);
        if was.is_some_and(|(_, _, d)| seen_only(&index_now(d), def) && !indexes_on(d)) {
            continue;
        }
        if was.is_none_or(|(_, _, d)| index_now(d) != *def || indexes_on(d)) {
            clauses.push(format!("add {def}"));
        }
    }
    for (name, _, def) in &to.checks {
        if from.checks.iter().find(|(n, _, _)| n == name).is_none_or(|(_, _, d)| d != def || checks_on(d)) {
            clauses.push(format!("add {def}"));
        }
    }
    for (key, value) in &to.options {
        if key != "charset" && from.option(key) != Some(value.as_str()) {
            clauses.push(format!("{key}={value}"));
        }
    }
    for (key, _) in &from.options {
        if to.option(key).is_none() {
            // An option of a number takes 0 for its default: MySQL's grammar refuses `default` there.
            clauses.push(match key.as_str() {
                "comment" => "comment=''".to_string(),
                "key_block_size" | "checksum" | "delay_key_write" | "max_rows" | "min_rows" | "avg_row_length" | "autoextend_size" => format!("{key}=0"),
                "engine_attribute" | "secondary_engine_attribute" | "connection" => format!("{key}=''"),
                // Those whose grammar has no `default`, each by the value it starts with.
                "compression" => "compression='None'".to_string(),
                "secondary_engine" => "secondary_engine=null".to_string(),
                "insert_method" => "insert_method=no".to_string(),
                "tablespace" => "tablespace=innodb_file_per_table".to_string(),
                _ => format!("{key}=default"),
            });
        }
    }
    // InnoDB adds one FULLTEXT index a statement: each after the first is a statement of its own.
    let fulltext: Vec<usize> = clauses.iter().enumerate().filter(|(_, c)| c.starts_with("add fulltext ")).map(|(k, _)| k).skip(1).collect();
    let mut later: Vec<String> = fulltext.iter().rev().map(|k| clauses.remove(*k)).collect::<Vec<_>>();
    later.reverse();
    // A table's partitioning changes apart from its body: where the body changes too, the old
    // goes first, since a key the body changes may leave out a column it partitions by, and the
    // new comes last, by the keys the body made. Alone, it is one statement.
    let mut out = Vec::new();
    if !early.is_empty() {
        out.push(format!("alter table {} {}", to.q, early.join(", ")));
    }
    if !first.is_empty() {
        out.push(format!("alter table {} {}", to.q, first.join(", ")));
    }
    if !late.is_empty() {
        out.push(format!("alter table {} {}", to.q, late.join(", ")));
    }
    // A partition taken out of a range or list table is dropped with its rows, first, in place:
    // what follows partitions what is left.
    let (dropped, kept) = dropped_partitions(&from.partition, &to.partition);
    if !dropped.is_empty() {
        out.push(format!("alter table {} drop partition {}", to.q, dropped.join(", ")));
    }
    let partition = kept.unwrap_or_else(|| from.partition.clone());
    let repartitions = if dropped.is_empty() { from.partition != to.partition } else { partition_list(&partition) != partition_list(&to.partition) };
    // A change to the partitions that MySQL makes in place, where partitioning the whole table again
    // copies every row while it blocks writes: in a statement after the table's own, unless that
    // changes a column the partitioning reads.
    let keeps_its_columns = from.columns.iter().filter(|c| reads(&from.partition, &c.name)).all(|c| !old_of.values().any(|o| *o == c.name) && to.column(&c.name).is_some_and(|now| now.def() == c.def()));
    let in_place = (repartitions && keeps_its_columns).then(|| partitions_changed(&partition, &to.partition)).flatten();
    let anew = repartitions && in_place.is_none();
    let apart = anew && !partition.is_empty() && (to.partition.is_empty() || !clauses.is_empty());
    if apart {
        out.push(format!("alter table {} remove partitioning", to.q));
    }
    match clauses.len() {
        0 => {}
        1 => out.push(format!("alter table {} {}", to.q, clauses[0])),
        _ => out.push(format!("alter table {}\n  {}", to.q, clauses.join(",\n  "))),
    }
    out.extend(later.iter().map(|c| format!("alter table {} {c}", to.q)));
    if let Some(change) = in_place {
        out.push(format!("alter table {} {change}", to.q));
    } else if anew && !to.partition.is_empty() {
        out.push(format!("alter table {} {}", to.q, to.partition));
    }
    out
}

/// A partitioning clause in parts: what comes before its list of partitions (`partition by range
/// (year(taken_on))`), and each partition's definition, as written. None where it lists none (`hash
/// (id) partitions 4`) or something follows the list.
fn partition_list(text: &str) -> Option<(String, Vec<String>)> {
    let tokens = tokenize(text, Dialect::Mysql);
    let mut depth = 0;
    let open = tokens.iter().enumerate().position(|(k, t)| {
        match t.text.as_str() {
            "(" if depth == 0 && tokens.get(k + 1).is_some_and(|n| n.upper() == "PARTITION") => return true,
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        false
    })?;
    let (mut parts, mut depth, mut start) = (Vec::new(), 0, tokens[open].end);
    for t in &tokens[open..] {
        match t.text.as_str() {
            "(" => depth += 1,
            "," if depth == 1 => {
                parts.push(text[start..t.start].trim().to_string());
                start = t.end;
            }
            ")" => {
                depth -= 1;
                if depth == 0 {
                    parts.push(text[start..t.start].trim().to_string());
                    return text[t.end..].trim().is_empty().then(|| (text[..tokens[open].start].trim().to_string(), parts));
                }
            }
            _ => {}
        }
    }
    None
}

/// The partitions of a range or list table that `new`, partitioning it the same way, no longer
/// names, as `old` writes them, and `old` without them; none where it names none of `old`'s, which
/// is the table partitioned anew.
fn dropped_partitions(old: &str, new: &str) -> (Vec<String>, Option<String>) {
    let (Some((head, a)), Some((other, b))) = (partition_list(old), partition_list(new)) else { return (vec![], None) };
    let kind = head.to_ascii_lowercase();
    if head != other || !(kind.starts_with("partition by range") || kind.starts_with("partition by list")) {
        return (vec![], None);
    }
    let named: Vec<String> = b.iter().map(|p| partition_key(p)).collect();
    let (gone, kept): (Vec<&String>, Vec<&String>) = a.iter().partition(|p| !named.contains(&partition_key(p)));
    if gone.is_empty() || kept.is_empty() {
        return (vec![], None);
    }
    let kept: Vec<&str> = kept.iter().map(|p| p.as_str()).collect();
    (gone.iter().map(|p| partition_name(p)).collect(), Some(format!("{head} ({})", kept.join(", "))))
}

/// A partition's name as MySQL compares it, unquoted and in lowercase.
fn partition_key(definition: &str) -> String {
    tokenize(definition, Dialect::Mysql).get(1).and_then(name_of).unwrap_or_default().to_lowercase()
}

/// A partition's name, as its definition writes it.
fn partition_name(definition: &str) -> String {
    tokenize(definition, Dialect::Mysql).get(1).map(|t| t.text.clone()).unwrap_or_default()
}

/// What a range partition's definition says it holds values below: `(2027)`, or `maxvalue`.
fn partition_bound(definition: &str) -> Option<String> {
    let tokens = tokenize(definition, Dialect::Mysql);
    let k = tokens.windows(3).position(|w| w[0].upper() == "VALUES" && w[1].upper() == "LESS" && w[2].upper() == "THAN")? + 3;
    let first = tokens.get(k)?;
    if first.text != "(" {
        return Some(first.text.to_ascii_lowercase());
    }
    let mut depth = 0;
    for t in &tokens[k..] {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(definition[first.start..t.end].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The clause that takes a range or list table's partitions from `old` to `new` in place, where the
/// way they are partitioned stays (subpartitions aside): partitions appended to the list are added
/// (`add partition`, which copies no rows and blocks no writes); a run of a range table's partitions
/// that changes is reorganized (`reorganize partition … into`, which copies only those partitions'
/// rows), with the partition after it where the run's last bound changes, so that it covers what it
/// did. A list table's other changes, and a partition removed, are None: MySQL's `reorganize` of
/// list partitions deletes the rows whose value the new lists lack, where partitioning the whole
/// table again fails on such a row.
fn partitions_changed(old: &str, new: &str) -> Option<String> {
    let ((head, a), (other, b)) = (partition_list(old)?, partition_list(new)?);
    let kind = head.to_ascii_lowercase();
    let range = kind.starts_with("partition by range");
    if head != other || !(range || kind.starts_with("partition by list")) || [old, new].iter().any(|t| t.to_ascii_lowercase().contains("subpartition")) {
        return None;
    }
    if b.len() > a.len() && b[..a.len()] == a[..] {
        return Some(format!("add partition ({})", b[a.len()..].join(", ")));
    }
    if !range {
        return None;
    }
    let before = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let after = a[before..].iter().rev().zip(b[before..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (mut run, mut into) = (a[before..a.len() - after].to_vec(), b[before..b.len() - after].to_vec());
    if into.is_empty() {
        return None;
    }
    // A run may change what it covers only at the table's end, and only to cover more.
    if run.is_empty() || run.last().and_then(|p| partition_bound(p)) != into.last().and_then(|p| partition_bound(p)) {
        if after > 0 {
            run.push(a[a.len() - after].clone());
            into.push(b[b.len() - after].clone());
        } else if run.is_empty() || into.last().and_then(|p| partition_bound(p)).as_deref() != Some("maxvalue") {
            return None;
        }
    }
    Some(format!("reorganize partition {} into ({})", run.iter().map(|p| partition_name(p)).collect::<Vec<_>>().join(", "), into.join(", ")))
}

/// What a table and its columns carry beside the body: the names of the table's foreign keys, and
/// of the indexes those keys made for themselves, each a JSON list.
const KEYS: &str = "foreign keys ";
/// A foreign key's line naming the columns it sets null, each `column <table>.<column>`, which MySQL
/// won't make NOT NULL while the key stands.
const SETS_NULL: &str = "sets null ";
const OWN: &str = "indexes of keys ";

/// The indexes a new foreign key makes as it is added, which the table's own change leaves to it.
fn made_by_keys(from: &[String], to: &[String]) -> BTreeSet<String> {
    let list = |extra: &[String], what: &str| -> BTreeSet<String> { extra.iter().find_map(|l| l.strip_prefix(what)).and_then(|l| serde_json::from_str::<Vec<String>>(l).ok()).unwrap_or_default().into_iter().collect() };
    let had = list(from, KEYS);
    list(to, OWN).into_iter().filter(|n| !had.contains(n)).collect()
}

/// MySQL's alterer. A plan's `alter table` runs under a strict sql_mode wherever the session's isn't
/// (`db::table_change_mode`), so a change to a type that can't hold one of a column's values fails on
/// it, and loses nothing, and a column made AUTO_INCREMENT keeps the 0s it holds.
#[derive(Default)]
pub struct MysqlAlterer {
    decided: super::Decided,
    /// The triggers both schemas have, which a plan may drop and make again.
    swaps: std::cell::RefCell<Vec<Swap>>,
    /// The columns, `column <table>.<column>`, of the first schema's foreign keys that set them null,
    /// which MySQL won't make NOT NULL while such a key stands.
    set_null: std::cell::RefCell<BTreeSet<String>>,
    /// Each NOT NULL step a table's statements start with, as written there and as the start of the
    /// plan writes it, under the table's name before any rename.
    early: std::cell::RefCell<Vec<(String, String)>>,
    /// Each NOT NULL step a key holds back, as written, its table as a statement names it, and its
    /// columns, each `column <table>.<column>`.
    late: std::cell::RefCell<Vec<(String, String, Vec<String>)>>,
    /// For each column of the first schema a foreign key reads, on either side, the statements that
    /// take such a key away: its own drop, and its table's.
    unkeyed_by: std::cell::RefCell<BTreeMap<String, Vec<String>>>,
}

impl MysqlAlterer {
    /// The columns of a table renamed between two of its bodies: the planner's decision where it
    /// made one, else its rule.
    fn pairs(&self, table: &str, from: &Body, to: &Body) -> Vec<(String, String)> {
        self.decided.of(table).unwrap_or_else(|| renames(from, to))
    }

    /// The statements of a table's change ([`alter_table`]) as the first schema names it `table`,
    /// with its NOT NULL step ([`held_columns`]) kept for [`Alterer::finish`] to move to the start of
    /// the plan: there it fails on a NULL before anything else runs, a foreign key's drop included,
    /// which MySQL commits on its own. A column under a key that sets it null keeps its step in
    /// place, after the key's drop, and joins that drop's statement where it is of the same table,
    /// which MySQL runs as one: a NULL then fails it with the key still in place.
    fn table_statements(&self, table: &str, from: &Body, to: &Body, pairs: &[(String, String)], made: &BTreeSet<String>) -> Vec<String> {
        let set_null = self.set_null.borrow();
        let held = held_columns(from, to, pairs);
        // InnoDB copies a table that has a FULLTEXT index to change it, and the copy refuses a column
        // made NOT NULL under any foreign key: such a table keeps every step in place.
        let copies = from.indexes.iter().any(|(_, _, def)| def.starts_with("fulltext "));
        let late: BTreeSet<String> = held.iter().filter(|(c, _)| copies || set_null.contains(&format!("column {table}.{c}"))).map(|(c, _)| c.clone()).collect();
        let early: Vec<&str> = held.iter().filter(|(c, _)| !late.contains(c)).map(|(_, clause)| clause.as_str()).collect();
        if !early.is_empty() {
            let clauses = early.join(", ");
            let moved = (format!("alter table {} {clauses}", to.q), format!("alter table {} {clauses}", from.q));
            let mut kept = self.early.borrow_mut();
            if !kept.contains(&moved) {
                kept.push(moved);
            }
        }
        let held_back: Vec<&str> = held.iter().filter(|(c, _)| late.contains(c)).map(|(_, clause)| clause.as_str()).collect();
        if !held_back.is_empty() {
            let columns = late.iter().map(|c| format!("column {table}.{c}")).collect();
            let joined = (format!("alter table {} {}", to.q, held_back.join(", ")), to.q.clone(), columns);
            let mut kept = self.late.borrow_mut();
            if !kept.contains(&joined) {
                kept.push(joined);
            }
        }
        alter_table(from, to, pairs, made, &late)
    }
}

/// A trigger both schemas have: the statement that drops it as the first has it, the one that makes
/// it as the second has it, and the tables it is on in each, as a statement names them.
#[derive(Debug)]
struct Swap {
    drop: String,
    create: String,
    tables: Vec<String>,
    /// The trigger's statement as the first schema makes it, whose names say what it reads.
    old: String,
}

/// The table a `create trigger` is on, as it names it: the name after its event's `on`.
fn trigger_table(create: &str) -> Option<String> {
    let tokens = tokenize(create, Dialect::Mysql);
    let at = tokens.windows(2).position(|w| matches!(w[0].upper().as_str(), "INSERT" | "UPDATE" | "DELETE") && w[1].upper() == "ON")?;
    tokens.get(at + 2).map(|t| t.text.clone())
}

/// The plan's triggers dropped and made again, written as one statement group under `lock tables`
/// on their tables. MySQL has no `create or replace trigger`, and commits each DDL statement on its
/// own: between a trigger's drop and its create, a write to its table would run without it, and if
/// the create failed the table would stay without it. Under the lock, a write waits for the group,
/// then runs with the new trigger. The drops move later, past what can't touch the triggers they
/// drop ([`crossable`]), and nothing else moves: the group holds the rest of the plan up to the last
/// create, in the plan's order, which makes what each new trigger needs before it, but with the
/// changes to tables first, so that one that fails, as a column's change can on a row, fails with
/// the old trigger still in place. Where the group would hold anything but triggers and changes to
/// tables, it isn't written so, and a comment above each drop says what runs without the trigger.
fn lock_swaps(swaps: &[Swap], statements: &mut Vec<String>) {
    let at = |s: &String, statements: &[String]| statements.iter().position(|t| t == s);
    let swapped: Vec<&Swap> = swaps.iter().filter(|w| matches!((at(&w.drop, statements), at(&w.create, statements)), (Some(d), Some(c)) if d < c)).collect();
    if swapped.is_empty() {
        return;
    }
    let drops: Vec<String> = statements.iter().filter(|s| swapped.iter().any(|w| w.drop == **s)).cloned().collect();
    let first_drop = statements.iter().position(|s| drops.contains(s)).expect("a drop");
    let moved: Vec<String> = statements.iter().filter(|s| !drops.contains(*s)).cloned().collect();
    let first = swapped.iter().filter_map(|w| at(&w.create, &moved)).min().expect("a create");
    let last = swapped.iter().filter_map(|w| at(&w.create, &moved)).max().expect("a create");
    let from = (first_drop..first).find(|k| !moved[*k].starts_with("--") && !crossable(&moved[*k], &swapped)).unwrap_or(first);
    // A comment among them goes above the group, which is one file.
    let (notes, held): (Vec<&String>, Vec<&String>) = moved[from..=last].iter().partition(|s| s.starts_with("--"));
    let (changes, made): (Vec<&String>, Vec<&String>) = held.into_iter().partition(|s| altered_table(s).is_some());
    let group: Vec<&String> = changes.into_iter().chain(drops.iter()).chain(made).collect();
    // Each table to lock, and whether the group writes it: a table only a foreign key it adds
    // references is read, to check the key, and stays readable. The names are the server's own, so
    // two that differ in case are two tables.
    let mut tables: Vec<(String, bool)> = Vec::new();
    let mut lock = |table: String, write: bool| match tables.iter_mut().find(|(t, _)| unquoted(t) == unquoted(&table)) {
        Some((_, w)) => *w |= write,
        None => tables.push((table, write)),
    };
    for s in &group {
        if let Some(w) = swapped.iter().find(|w| w.drop == **s || w.create == **s) {
            w.tables.iter().for_each(|t| lock(t.clone(), true));
        } else if let Some(table) = is_trigger_create(s).then(|| trigger_table(s)).flatten() {
            lock(table, true);
        } else if let Some(table) = altered_table(s) {
            lock(table, true);
            referenced_tables(s).into_iter().for_each(|t| lock(t, false));
        } else {
            for w in &swapped {
                let name = w.drop.trim_start_matches("drop trigger ");
                let k = statements.iter().position(|s| *s == w.drop).expect("a drop");
                statements.insert(k, format!("-- Until {name} is made again, writes to {} run without it", w.tables.first().map(String::as_str).unwrap_or("its table")));
            }
            return;
        }
    }
    let tables: Vec<String> = tables.into_iter().map(|(t, write)| format!("{t} {}", if write { "write" } else { "read" })).collect();
    let body: Vec<&str> = group.iter().map(|s| s.trim_end().trim_end_matches(';')).collect();
    let locked = format!("lock tables {};\n{};\nunlock tables", tables.join(", "), body.join(";\n"));
    *statements = moved[..from].iter().cloned().chain(notes.into_iter().cloned()).chain([locked]).chain(moved[last + 1..].iter().cloned()).collect();
}

/// Whether a statement can run while a swapped trigger still runs, before its drop, and leave it as
/// it works: one on an object none of the swapped triggers is on or names, or that makes a new one.
fn crossable(statement: &str, swapped: &[&Swap]) -> bool {
    let tokens = tokenize(statement, Dialect::Mysql);
    let words: Vec<String> = tokens.iter().take(5).map(Token::upper).collect();
    let named = |object: &str| swapped.iter().any(|w| w.tables.iter().any(|t| unquoted(t).eq_ignore_ascii_case(object)) || tokenize(&w.old, Dialect::Mysql).iter().filter_map(name_of).any(|n| n.eq_ignore_ascii_case(object)));
    let replaces = words.iter().take(3).any(|w| w == "REPLACE");
    // What the statement acts on: a table's change, an object dropped or renamed, or one made.
    let object = match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["CREATE", ..] if !replaces => return true,
        ["ALTER", "TABLE", ..] | ["RENAME", "TABLE", ..] => tokens.get(2),
        ["DROP", _, "IF", "EXISTS", ..] => tokens.get(4),
        ["DROP", ..] => tokens.get(2),
        ["CREATE", "OR", "REPLACE", ..] => tokens.iter().skip(3).skip_while(|t| !matches!(t.upper().as_str(), "VIEW" | "FUNCTION" | "PROCEDURE")).nth(1),
        _ => None,
    };
    object.and_then(name_of).is_some_and(|o| !named(&o))
}

/// A name as a statement writes it, read back: its quotes taken off, as MySQL reads it.
fn unquoted(name: &str) -> String {
    tokenize(name, Dialect::Mysql).first().and_then(name_of).unwrap_or_else(|| name.to_string())
}

/// The tables a statement's foreign keys reference, as it names them.
fn referenced_tables(statement: &str) -> Vec<String> {
    let tokens = tokenize(statement, Dialect::Mysql);
    tokens.windows(2).filter(|w| w[0].upper() == "REFERENCES").map(|w| w[1].text.clone()).collect()
}

/// The table an `alter table` changes, as it names it.
fn altered_table(statement: &str) -> Option<String> {
    let tokens = tokenize(statement, Dialect::Mysql);
    (tokens.len() > 2 && tokens[0].upper() == "ALTER" && tokens[1].upper() == "TABLE").then(|| tokens[2].text.clone())
}

/// Whether a statement makes a trigger, whatever its definer.
fn is_trigger_create(s: &str) -> bool {
    let words: Vec<String> = tokenize(s, Dialect::Mysql).iter().take_while(|t| t.upper() != "ON").map(Token::upper).collect();
    words.first().is_some_and(|w| w == "CREATE") && words.iter().any(|w| w == "TRIGGER")
}

/// `text` with each name after its first parenthesis that names one of `columns` in another case
/// written as the column is named. A bare word MySQL reserves is a keyword (`as`), never a column,
/// which a statement would quote.
fn columns_as_named(text: &str, columns: &[Column], reserved: &BTreeSet<String>) -> String {
    let Some(open) = text.find('(') else { return text.to_string() };
    let mut out = text[..open].to_string();
    let mut at = open;
    for t in tokenize(&text[open..], Dialect::Mysql) {
        let (start, end) = (open + t.start, open + t.end);
        out.push_str(&text[at..start]);
        let bare = match t.kind {
            TokenKind::Word if !reserved.contains(&t.text.to_ascii_uppercase()) => Some(t.text.clone()),
            TokenKind::Quoted => Some(t.text.trim_matches('`').replace("``", "`")),
            _ => None,
        };
        match bare.and_then(|b| columns.iter().find(|c| c.name != b && c.name.eq_ignore_ascii_case(&b))) {
            Some(c) => out.push_str(&c.q),
            None => out.push_str(&text[start..end]),
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// Whether a definition reads the column named `column`, by any token naming it.
fn reads(text: &str, column: &str) -> bool {
    tokenize(text, Dialect::Mysql).iter().any(|t| match t.kind {
        TokenKind::Word => t.text.eq_ignore_ascii_case(column),
        TokenKind::Quoted => t.text.trim_matches('`').replace("``", "`").eq_ignore_ascii_case(column),
        _ => false,
    })
}

/// An index's definition without its visibility, and whether it is invisible.
fn unseen(def: &str) -> (&str, bool) {
    match def.strip_suffix(" invisible") {
        Some(base) => (base, true),
        None => (def, false),
    }
}

/// A MySQL column type, as information_schema writes it, for a change that may lose values.
fn type_class(def: &str) -> super::TypeClass {
    use super::TypeClass;
    let lower = def.trim().to_lowercase();
    let Some(c) = regex::Regex::new(r"^([a-z]+)(?:\(([0-9]+)(?:,([0-9]+))?\))?").unwrap().captures(&lower) else { return TypeClass::Other };
    let (p, s) = (c.get(2).and_then(|m| m.as_str().parse::<u32>().ok()), c.get(3).and_then(|m| m.as_str().parse::<u32>().ok()));
    match &c[1] {
        "tinyint" => TypeClass::Int(8),
        "smallint" => TypeClass::Int(16),
        "mediumint" => TypeClass::Int(24),
        "int" | "integer" => TypeClass::Int(32),
        "bigint" => TypeClass::Int(64),
        // A float with a scale (`double(10,2)`) keeps that many decimal places.
        "float" | "double" | "real" if s.is_some() => TypeClass::Numeric(s),
        "float" => TypeClass::Float(6),
        "double" | "real" => TypeClass::Float(15),
        "decimal" | "numeric" => TypeClass::Numeric(Some(s.unwrap_or(0))),
        "date" => TypeClass::Date,
        // A timestamp is a moment, kept in UTC; a datetime is a wall-clock time.
        "datetime" => TypeClass::Timestamp { zone: false, precision: p.unwrap_or(0) },
        "timestamp" => TypeClass::Timestamp { zone: true, precision: p.unwrap_or(0) },
        "time" => TypeClass::Time { precision: p.unwrap_or(0) },
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" => TypeClass::Text,
        "json" => TypeClass::Jsonb,
        _ => TypeClass::Other,
    }
}

/// A MySQL type's name alone, lowercase: `varchar` of `varchar(20)`.
fn base_type(ty: &str) -> String {
    ty.trim().chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase()
}

/// The type a column's definition opens with: `decimal(10,2) unsigned` of `decimal(10,2) unsigned not null`.
fn type_of(def: &str) -> String {
    let words: Vec<&str> = def.split_whitespace().collect();
    let n = 1 + words.iter().skip(1).take_while(|w| matches!(**w, "unsigned" | "zerofill" | "signed")).count();
    words[..n.min(words.len())].join(" ")
}

/// A `grant` or `revoke` of the planner's split where MySQL reads it: whether it grants, its
/// privilege, and what follows it (`on … to …`), each outside backticks.
fn privilege_parts(statement: &str) -> Option<(bool, &str, &str)> {
    let (grant, rest) = match (statement.strip_prefix("grant "), statement.strip_prefix("revoke ")) {
        (Some(rest), _) => (true, rest),
        (_, Some(rest)) => (false, rest),
        _ => return None,
    };
    let mut quoted = false;
    for (k, c) in rest.char_indices() {
        match c {
            '`' => quoted = !quoted,
            ' ' if !quoted && rest[k..].starts_with(" on ") => return Some((grant, &rest[..k], &rest[k..])),
            _ => {}
        }
    }
    None
}

impl Alterer for MysqlAlterer {
    fn schemas(&self, from: &Schema, to: &Schema) {
        let swaps = from
            .objects
            .values()
            .filter(|o| o.kind == Some(Kind::Trigger) && o.owner.is_some())
            .filter_map(|a| {
                let b = to.objects.get(&a.id)?;
                let (drop, create) = (a.drop.first()?.clone(), b.create.first()?.clone());
                let old = a.create.first()?.clone();
                let tables = [trigger_table(&old), trigger_table(&create)].into_iter().flatten().collect();
                Some(Swap { drop, create, tables, old })
            })
            .collect();
        *self.swaps.borrow_mut() = swaps;
        *self.early.borrow_mut() = Vec::new();
        *self.late.borrow_mut() = Vec::new();
        let mut unkeyed_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for o in from.objects.values().filter(|o| o.kind == Some(Kind::Constraint) && o.id.starts_with("foreign key ")) {
            let table_drop = o.owner.as_ref().and_then(|t| from.objects.get(t)).and_then(|t| t.drop.first());
            let ways: Vec<String> = o.drop.first().into_iter().chain(table_drop).cloned().collect();
            for column in o.deps.iter().filter(|d| d.starts_with("column ")) {
                unkeyed_by.entry(column.clone()).or_default().extend(ways.iter().cloned());
            }
        }
        *self.unkeyed_by.borrow_mut() = unkeyed_by;
        *self.set_null.borrow_mut() = from.objects.values().flat_map(|o| o.extra.iter().filter_map(|l| l.strip_prefix(SETS_NULL)).flat_map(|l| serde_json::from_str::<Vec<String>>(l).unwrap_or_default())).collect();
    }

    /// The NOT NULL steps a table's change starts with go first, or join a key's drop where the key
    /// holds them back ([`MysqlAlterer::table_statements`]);
    /// a trigger dropped and made again goes under `lock tables` ([`lock_swaps`]); then the
    /// privileges granted (or revoked) on one object to one account, which MySQL takes in one
    /// statement, written as one where the plan has them one after another: `grant select, update
    /// (email) on users to … with grant option`. Only those side by side: one further on may wait
    /// for what the plan makes between them (a column it grants on).
    fn finish(&self, statements: &mut Vec<String>) {
        // A table's held step joins the last drop of its own keys before it, which MySQL runs as one
        // statement, placed after every statement that takes a key on the column away (a key that
        // references it, or that key's table), which the step waits for. The drop moves down past
        // those, and past other keys' drops alone, none of which waits for another's: anything else
        // between may wait for the drop (the table its key references, dropped), and then the two
        // stay apart, as the plan has them.
        let unkeyed_by = self.unkeyed_by.borrow();
        let drops_a_key = |s: &str| {
            let t = tokenize(s, Dialect::Mysql);
            let clause = |k: usize| t.get(k).map(Token::upper).unwrap_or_default();
            t.len() > 5 && clause(0) == "ALTER" && clause(1) == "TABLE" && clause(3) == "DROP" && clause(4) == "FOREIGN" && clause(5) == "KEY" && !t.iter().any(|w| w.upper() == "MODIFY")
        };
        for (here, table, columns) in self.late.borrow().iter() {
            let Some(at) = statements.iter().position(|s| s == here) else { continue };
            let drop = format!("alter table {table} drop foreign key ");
            let Some(key) = statements[..at].iter().rposition(|s| s.starts_with(&drop)) else { continue };
            let ways: Vec<&String> = columns.iter().flat_map(|c| unkeyed_by.get(c).into_iter().flatten()).collect();
            let unkeys = |s: &str| ways.iter().any(|w| s == w.as_str() || s.starts_with(&format!("{w}, ")));
            let after = statements[key + 1..at].iter().rposition(|s| unkeys(s)).map(|k| key + 1 + k);
            let clauses = here[format!("alter table {table} ").len()..].to_string();
            let joined = format!("{}, {clauses}", statements[key]);
            match after {
                Some(last) if statements[key + 1..=last].iter().all(|s| unkeys(s) || drops_a_key(s)) => {
                    statements.remove(at);
                    statements.insert(last + 1, joined);
                    statements.remove(key);
                }
                Some(_) => {}
                None => {
                    statements.remove(at);
                    statements[key] = joined;
                }
            }
        }
        // The NOT NULL steps no key holds back go first, before any key goes.
        let mut front = Vec::new();
        for (here, first) in self.early.borrow().iter() {
            if let Some(at) = statements.iter().position(|s| s == here) {
                statements.remove(at);
                front.push(first.clone());
            }
        }
        statements.splice(0..0, front);
        lock_swaps(&self.swaps.borrow(), statements);
        // Each statement, or a run of grants (or of revokes) on one object to one account.
        let mut runs: Vec<(String, Option<(bool, Vec<String>, String)>)> = Vec::new();
        for s in statements.drain(..) {
            let Some((grant, privilege, rest)) = privilege_parts(&s) else {
                runs.push((s, None));
                continue;
            };
            if let Some((_, Some((g, privileges, r)))) = runs.last_mut() {
                if *g == grant && r.as_str() == rest {
                    privileges.push(privilege.to_string());
                    continue;
                }
            }
            runs.push((String::new(), Some((grant, vec![privilege.to_string()], rest.to_string()))));
        }
        *statements = runs
            .into_iter()
            .map(|(s, run)| match run {
                None => s,
                Some((grant, privileges, rest)) => {
                    let option = privileges.iter().any(|p| p == "grant option");
                    let others: Vec<&String> = privileges.iter().filter(|p| *p != "grant option").collect();
                    let list = others.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ");
                    match (grant, option, others.is_empty()) {
                        (true, true, false) => format!("grant {list}{rest} with grant option"),
                        (_, true, false) => format!("revoke {list}, grant option{rest}"),
                        (verb, _, _) => format!("{} {}{rest}", if verb { "grant" } else { "revoke" }, privileges.join(", ")),
                    }
                }
            })
            .collect();
    }

    fn narrows(&self, from: &Object, to: &Object) -> Option<String> {
        if from.kind != Some(Kind::Column) {
            return None;
        }
        // A column that stops being virtual keeps none of the values it computed, and one that
        // becomes generated computes its values in place of those it holds.
        let generated = |def: &str| {
            let def = def.to_ascii_lowercase();
            def.find("generated always as").map(|at| if def[at..].contains(" virtual") { "virtual" } else { "stored" })
        };
        match (generated(&from.def), generated(&to.def)) {
            (Some("virtual"), None) => return Some("stops being a virtual column, which leaves its rows without the values it computed".into()),
            (None, Some(_)) => return Some("becomes a generated column, which replaces its values with the expression's".into()),
            _ => {}
        }
        let (a, b) = (type_of(&from.def), type_of(&to.def));
        // Strict mode fails a change on a value the new type can't hold, but a `char` drops a
        // value's trailing spaces as it reads it, and a `binary` pads a value with zero bytes, both
        // without a word.
        let (x, y) = (base_type(&a), base_type(&b));
        if y == "char" && matches!(x.as_str(), "varchar" | "tinytext" | "text" | "mediumtext" | "longtext") {
            return Some(format!("changes from {a} to {b}, which drops the trailing spaces of its values"));
        }
        // A shorter `binary` fails on a value too long for it, as any narrower type does.
        let length = |t: &str| t.split(['(', ')']).nth(1).and_then(|n| n.trim().parse::<u32>().ok()).unwrap_or(1);
        if y == "binary" && (x != "binary" || length(&b) > length(&a)) {
            return Some(format!("changes from {a} to {b}, which pads its values with zero bytes to its length"));
        }
        super::narrowing(&type_class(&a), &type_class(&b)).map(|why| format!("changes from {a} to {b}, which {why}"))
    }

    fn decide(&self, renames: &[(String, String)]) {
        self.decided.set(renames);
    }

    /// The partitions a table's change drops with their rows, each `partition <table>.<partition>`.
    fn loses(&self, from: &Object, to: &Object) -> Vec<String> {
        let (Some(a), Some(b), Some(table)) = (Body::from_json(&from.def), Body::from_json(&to.def), from.id.strip_prefix("table ")) else { return vec![] };
        if from.kind != Some(Kind::Table) {
            return vec![];
        }
        let (dropped, _) = dropped_partitions(&a.partition, &b.partition);
        dropped.iter().map(|p| format!("partition {table}.{}", tokenize(p, Dialect::Mysql).first().and_then(name_of).unwrap_or_else(|| p.clone()))).collect()
    }

    /// A column renamed: the table's whole statement, with the rename among its clauses, which
    /// keeps every other change after the rename in one statement. Where a table renames several
    /// columns, the last rename carries it (the planner takes them in the order of their names),
    /// and the others none.
    fn rename(&self, from: &Object, to: &Object) -> Option<Vec<String>> {
        let (a, b) = (Body::from_json(from.extra.first()?)?, Body::from_json(to.extra.first()?)?);
        let table = to.owner.as_deref()?.strip_prefix("table ")?;
        let new = to.id.strip_prefix(&format!("column {table}."))?;
        let pairs = self.pairs(table, &a, &b);
        if !pairs.iter().any(|(_, n)| n == new) {
            return None;
        }
        let last = pairs.iter().map(|(_, n)| n.as_str()).max()?;
        let was = from.owner.as_deref()?.strip_prefix("table ")?;
        Some(if new == last { self.table_statements(was, &a, &b, &pairs, &made_by_keys(&from.extra, &to.extra)) } else { Vec::new() })
    }

    fn alter(&self, from: &Object, to: &Object) -> Option<Vec<String>> {
        match to.kind? {
            Kind::Table => {
                let (a, b) = (Body::from_json(&from.def)?, Body::from_json(&to.def)?);
                let pairs = self.pairs(to.id.strip_prefix("table ")?, &a, &b);
                // Where a column is renamed, the rename writes the table's statement.
                Some(if pairs.is_empty() { self.table_statements(from.id.strip_prefix("table ")?, &a, &b, &pairs, &made_by_keys(&from.extra, &to.extra)) } else { Vec::new() })
            }
            // The table's statement changes the column.
            Kind::Column => Some(Vec::new()),
            Kind::View => Some(vec![to.create.first()?.replacen("create ", "create or replace ", 1)]),
            // A routine or an event: MySQL has no create or replace for either, and nothing holds
            // one, so it is dropped and made again in place.
            Kind::Function | Kind::Trigger if to.owner.is_none() => Some(from.drop.iter().chain(&to.create).cloned().collect()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx { db: "app".into(), reserved: ["SELECT", "KEY", "ORDER"].iter().map(|s| s.to_string()).collect(), user: "root@localhost".into(), engine: "InnoDB".into(), collation: "utf8mb4_0900_ai_ci".into() }
    }

    #[test]
    fn tidies_the_servers_rendering() {
        let c = ctx();
        assert_eq!(c.tidy("select `app`.`users`.`id` AS `id`,`app`.`users`.`order` AS `order` from `app`.`users`", SERVER_TEXT), "select users.id as id, users.`order` as `order` from users");
        assert_eq!(c.tidy("`n` int NOT NULL DEFAULT '0' /*!80023 INVISIBLE */", SERVER_TEXT), "n int not null default '0' invisible");
        // A quoted name before a parenthesis is a stored function's, which a bare name is not.
        assert_eq!(c.tidy("`count`(`x`) + b'101'", SERVER_TEXT), "`count`(x) + b'101'");
        assert_eq!(c.tidy("insert into app.log values ('app.x')", BODY_TEXT), "insert into log values ('app.x')");
        assert_eq!(stored_text(r#"json_schema_valid(_utf8mb4\'{"enum":["it\\\'s","cafÃ©"]}\',`w`)"#), r#"json_schema_valid(_utf8mb4'{"enum":["it\'s","café"]}',`w`)"#);
        assert_eq!(generated_from("int GENERATED ALWAYS AS ((`n` * 2)) VIRTUAL", "(`n` * 3)"), "int GENERATED ALWAYS AS ((`n` * 3)) VIRTUAL");
        assert_eq!(column_rest("character set latin1 collate latin1_bin default null comment 'x'"), "comment 'x'");
        assert_eq!(column_rest("collate utf8mb4_bin default (cast(_utf8mb4'foo' as char charset binary))"), "default (cast(_utf8mb4'foo' as char charset binary))");
    }

    #[test]
    fn reads_a_table_body() {
        let c = ctx();
        let text = "CREATE TABLE `t` (\n  `id` int NOT NULL,\n  `b` varchar(20) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin DEFAULT 'x',\n  PRIMARY KEY (`id`),\n  KEY `k` (`b`(4) DESC),\n  CONSTRAINT `c` CHECK ((`id` > 0))\n) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci COMMENT='People'\n/*!50100 PARTITION BY HASH (`id`)\nPARTITIONS 2 */";
        let body = parse_table(&c, text, "t", &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(body.columns.len(), 2);
        assert_eq!(body.indexes, [("PRIMARY".to_string(), String::new(), "primary key (id)".to_string()), ("k".to_string(), "k".to_string(), "key k (b(4) desc)".to_string())]);
        assert_eq!(body.checks, [("c".to_string(), "c".to_string(), "constraint c check ((id > 0))".to_string())]);
        assert_eq!(body.option("comment"), Some("'People'"));
        assert_eq!(body.option("auto_increment"), None);
        assert!(body.counter.is_some());
        assert_eq!(body.partition, "partition by hash (id) partitions 2");
    }

    #[test]
    fn makes_a_column_not_null_before_it_counts() {
        let column = |name: &str, rest: &str| Column { name: name.into(), q: name.into(), ty: "int".into(), collation: String::new(), rest: rest.into() };
        let table = |c: Column| Body { q: "t".into(), indexes: vec![(c.name.clone(), c.q.clone(), format!("key {} ({})", c.q, c.q))], columns: vec![column("id", "not null"), c], ..Body::default() };
        let to = table(column("ticket", "not null auto_increment comment 'keep null rows'"));
        // Renamed as it counts: made NOT NULL as it was, under the name it had; a comment's words
        // are no attribute.
        let plan = alter_table(&table(column("seq", "comment 'not null until rollout'")), &to, &[("seq".into(), "ticket".into())], &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(plan.first().map(String::as_str), Some("alter table t modify column seq int comment 'not null until rollout' not null"), "{plan:?}");
        // NOT NULL already: nothing before.
        let plan = alter_table(&table(column("ticket", "not null comment 'x'")), &to, &[], &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(plan.len(), 1, "{plan:?}");
    }

    #[test]
    fn changes_partitions_in_place_where_mysql_can() {
        let range = |parts: &[(&str, &str)]| format!("partition by range (year(taken_on)) ({})", parts.iter().map(|(n, b)| format!("partition {n} values less than {b} engine = innodb")).collect::<Vec<_>>().join(", "));
        let (p26, p27, p28, max) = (("p2026", "(2027)"), ("p2027", "(2028)"), ("p2028", "(2029)"), ("pmax", "maxvalue"));
        // Appended: added, with no row copied.
        assert_eq!(partitions_changed(&range(&[p26, p27]), &range(&[p26, p27, p28, max])).as_deref(), Some("add partition (partition p2028 values less than (2029) engine = innodb, partition pmax values less than maxvalue engine = innodb)"));
        // Before the last: the partition after takes part, so the run covers what it did.
        assert_eq!(partitions_changed(&range(&[p26, p27, max]), &range(&[p26, p27, p28, max])).as_deref(), Some("reorganize partition pmax into (partition p2028 values less than (2029) engine = innodb, partition pmax values less than maxvalue engine = innodb)"));
        let (p10, p30) = (("p10", "(10)"), ("p30", "(30)"));
        assert_eq!(partitions_changed(&range(&[p10, ("p20", "(20)"), p30]), &range(&[p10, ("p20", "(15)"), p30])).as_deref(), Some("reorganize partition p20, p30 into (partition p20 values less than (15) engine = innodb, partition p30 values less than (30) engine = innodb)"));
        // The last made to hold every value above it.
        assert_eq!(partitions_changed(&range(&[p26, p27]), &range(&[p26, ("p2027", "maxvalue")])).as_deref(), Some("reorganize partition p2027 into (partition p2027 values less than maxvalue engine = innodb)"));
        // Covering less at the end, a partition removed, a list's change but an appended partition,
        // hash and subpartitions: none.
        assert_eq!(partitions_changed(&range(&[p26, p27, max]), &range(&[p26, p27])), None);
        assert_eq!(partitions_changed(&range(&[p26, p27, p28]), &range(&[p27, p28])), None);
        let list = |parts: &[&str]| format!("partition by list columns(region) ({})", parts.iter().map(|p| format!("partition {p} engine = innodb")).collect::<Vec<_>>().join(", "));
        assert_eq!(partitions_changed(&list(&["pa values in ('a', 'b')"]), &list(&["pa values in ('a', 'b')", "pc values in ('c')"])).as_deref(), Some("add partition (partition pc values in ('c') engine = innodb)"));
        assert_eq!(partitions_changed(&list(&["pa values in ('a', 'b')", "pc values in ('c')"]), &list(&["pa values in ('a')", "pc values in ('c')"])), None);
        assert_eq!(partitions_changed("partition by hash (id) partitions 2", "partition by hash (id) partitions 4"), None);
        // A partition taken out is dropped, with its rows; partitioned another way, or taken out
        // with every other, the table is partitioned anew.
        assert_eq!(dropped_partitions(&range(&[p26, p27, max]), &range(&[p27, max])), (vec!["p2026".to_string()], Some(range(&[p27, max]))));
        assert_eq!(dropped_partitions(&range(&[p26, p27, p28]), &range(&[p26, p28, max])).0, ["p2027"]);
        assert_eq!(dropped_partitions(&list(&["pa values in ('a')", "`pB` values in ('b')"]), &list(&["pa values in ('a')"])).0, ["`pB`"]);
        assert_eq!(dropped_partitions(&range(&[p26, p27]), &range(&[p26, p27]).replace("year(taken_on))", "taken_on)")).0, Vec::<String>::new());
        assert_eq!(dropped_partitions(&range(&[p26]), &range(&[p27])).0, Vec::<String>::new());
        assert_eq!(dropped_partitions(&range(&[p26, p27]), "").0, Vec::<String>::new());
        assert_eq!(partitions_changed(&range(&[p26]), &range(&[p26, p27]).replace("year(taken_on))", "taken_on)")), None);
    }

    #[test]
    fn swaps_a_trigger_under_a_lock_on_its_table() {
        let swap = |name: &str, table: &str, body: &str| Swap {
            drop: format!("drop trigger {name}"),
            create: format!("create trigger {name} before update on {table} for each row {body}"),
            tables: vec![table.into()],
            old: format!("create trigger {name} before update on {table} for each row set new.n = old.n + f(1)"),
        };
        let swapped = |plan: &[&str]| {
            let mut plan: Vec<String> = plan.iter().map(|s| s.to_string()).collect();
            lock_swaps(&[swap("a_touch", "a", "set new.m = 2"), swap("b_touch", "b", "set new.n = 3")], &mut plan);
            plan
        };
        // The drops wait past what no trigger names; a change to a table one is on or names goes in
        // the group, first.
        assert_eq!(
            swapped(&["drop trigger a_touch", "drop trigger b_touch", "drop view v", "alter table a add column m int", "create trigger a_touch before update on a for each row set new.m = 2", "create trigger a_new after insert on a for each row set @x = 1", "create trigger b_touch before update on b for each row set new.n = 3", "create view w as select 1"]),
            [
                "drop view v",
                "lock tables a write, b write;\nalter table a add column m int;\ndrop trigger a_touch;\ndrop trigger b_touch;\ncreate trigger a_touch before update on a for each row set new.m = 2;\ncreate trigger a_new after insert on a for each row set @x = 1;\ncreate trigger b_touch before update on b for each row set new.n = 3;\nunlock tables",
                "create view w as select 1",
            ]
        );
        // A change that renames a column the trigger reads, and its comment above.
        assert_eq!(
            swapped(&["drop trigger a_touch", "-- n is renamed m, keeping its rows.", "alter table a rename column n to m", "create trigger a_touch before update on a for each row set new.m = 2"]),
            ["-- n is renamed m, keeping its rows.", "lock tables a write;\nalter table a rename column n to m;\ndrop trigger a_touch;\ncreate trigger a_touch before update on a for each row set new.m = 2;\nunlock tables"]
        );
        // Every change between, in the plan's order, so the new trigger has what it needs.
        assert_eq!(
            swapped(&["drop trigger a_touch", "alter table a remove partitioning", "alter table a rename column n to m", "alter table z add column v int", "alter table a partition by list (id) (partition p values in (1))", "create trigger a_touch before update on a for each row set new.m = 2"]),
            ["lock tables a write, z write;\nalter table a remove partitioning;\nalter table a rename column n to m;\nalter table z add column v int;\nalter table a partition by list (id) (partition p values in (1));\ndrop trigger a_touch;\ncreate trigger a_touch before update on a for each row set new.m = 2;\nunlock tables"]
        );
        // A foreign key added locks the table it references too, to read; one that differs in case is
        // another table.
        assert_eq!(
            swapped(&["drop trigger a_touch", "alter table a add constraint a_x foreign key (x) references `order` (id)", "create trigger a_touch before update on a for each row set new.m = 2"]),
            ["lock tables a write, `order` read;\nalter table a add constraint a_x foreign key (x) references `order` (id);\ndrop trigger a_touch;\ncreate trigger a_touch before update on a for each row set new.m = 2;\nunlock tables"]
        );
        assert!(swapped(&["drop trigger a_touch", "alter table a add constraint a_x foreign key (x) references A (id)", "create trigger a_touch before update on a for each row set new.m = 2"])[0].starts_with("lock tables a write, A read;\n"));
        assert!(swapped(&["drop trigger a_touch", "alter table a add constraint a_x foreign key (x) references `a``` (id)", "create trigger a_touch before update on a for each row set new.m = 2"])[0].starts_with("lock tables a write, `a``` read;\n"));
        // So would a function it calls, which isn't a table's change: written apart, saying so.
        let plan = swapped(&["drop trigger a_touch", "drop function f", "create function f () returns int return 2", "create trigger a_touch before update on a for each row set new.m = 2"]);
        assert_eq!(plan[0], "-- Until a_touch is made again, writes to a run without it");
        assert_eq!(plan.len(), 5);
    }
}
