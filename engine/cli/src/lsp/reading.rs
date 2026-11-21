//! What the language server reads off a statement's tokens, the same in every dialect: which tables and aliases each query of it names, its WITH queries, the columns of its result, and the tables a project's SQL files create.

use syntax::{Dialect, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Word,
    Quoted,
    String,
    Number,
    Variable,
    Punct,
}

#[derive(Clone, Copy, Debug)]
pub struct Tok<'s> {
    pub text: &'s str,
    pub start: u32,
    pub end: u32,
    pub class: Class,
}

impl Tok<'_> {
    pub fn is(&self, word: &str) -> bool {
        self.class == Class::Word && self.text.eq_ignore_ascii_case(word)
    }

    pub fn is_any(&self, words: &[&str]) -> bool {
        self.class == Class::Word && words.iter().any(|w| self.text.eq_ignore_ascii_case(w))
    }

    pub fn is_punct(&self, p: &str) -> bool {
        self.class == Class::Punct && self.text == p
    }

    pub fn is_name(&self) -> bool {
        matches!(self.class, Class::Word | Class::Quoted)
    }

    pub fn at(&self) -> (u32, u32) {
        (self.start, self.end)
    }

    /// The name it spells, without its quotes. One still being typed has no closing quote.
    pub fn name(&self) -> String {
        if self.class != Class::Quoted {
            return self.text.to_string();
        }
        let open = self.text.as_bytes()[0];
        let close = if open == b'[' { b']' } else { open };
        let inner = &self.text[1..];
        let inner = if inner.as_bytes().last() == Some(&close) { &inner[..inner.len() - 1] } else { inner };
        let close = close as char;
        inner.replace(&format!("{close}{close}"), &close.to_string())
    }
}

/// A name in `dialect`'s case folding: Postgres and SQLite fold ASCII alone.
fn folded(dialect: Dialect, name: &str) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => name.to_ascii_lowercase(),
        _ => name.to_lowercase(),
    }
}

/// How `dialect` compares a name as written: Postgres folds what isn't quoted to lower case and keeps what is; the others compare without case.
pub fn key(dialect: Dialect, t: &Tok<'_>) -> String {
    if dialect == Dialect::Postgres && t.class == Class::Quoted {
        t.name()
    } else {
        folded(dialect, &t.name())
    }
}

/// How `dialect` compares a name as its catalog holds it.
pub fn catalog_key(dialect: Dialect, name: &str) -> String {
    if dialect == Dialect::Postgres {
        name.to_string()
    } else {
        folded(dialect, name)
    }
}

/// A column read off a query's text, with its name as `dialect` compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub key: String,
}

fn named(dialect: Dialect, t: &Tok<'_>) -> Named {
    Named { name: t.name(), key: key(dialect, t) }
}

/// Words that are never a column's bare name.
pub const RESERVED: &[&str] = &[
    "ALL", "AND", "AS", "ASC", "BETWEEN", "BY", "CASE", "CROSS", "DELETE", "DESC", "DISTINCT", "ELSE", "END", "EXCEPT", "EXISTS", "FALSE", "FROM", "FULL", "GROUP", "HAVING", "IN", "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN", "LEFT", "LIKE", "LIMIT", "NATURAL", "NOT", "NULL", "OFFSET", "ON", "OR", "ORDER", "OUTER", "RETURNING", "RIGHT", "SELECT", "SET", "THEN", "TRUE", "UNION", "UPDATE", "USING", "VALUES", "WHEN", "WHERE", "WITH",
];

fn class_of(dialect: Dialect, text: &str) -> Class {
    let b = text.as_bytes();
    let next = b.get(1).copied().unwrap_or(0);
    let names = next.is_ascii_alphanumeric() || next == b'_' || next >= 0x80;
    match b[0] {
        b'\'' => Class::String,
        // `$$ … $$` and `$tag$ … $tag$`.
        b'$' if text.len() >= 2 && text.ends_with('$') && text[1..].contains('$') => Class::String,
        b'"' if dialect == Dialect::Mysql => Class::String,
        b'"' | b'`' => Class::Quoted,
        b'[' if matches!(dialect, Dialect::Sqlite | Dialect::SqlServer) && text.len() >= 2 => Class::Quoted,
        b'0'..=b'9' => Class::Number,
        b'.' if next.is_ascii_digit() => Class::Number,
        // On Postgres `?` and `@>` are operators.
        b'?' if dialect != Dialect::Postgres => Class::Variable,
        b'@' if names || next == b'@' || next == b'\'' || next == b'"' || next == b'`' => Class::Variable,
        b':' | b'$' if names => Class::Variable,
        b'#' if dialect == Dialect::SqlServer => Class::Word,
        c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
            if text.contains('\'') {
                Class::String
            } else {
                Class::Word
            }
        }
        _ => Class::Punct,
    }
}

/// `text` with each of Reylo's `:name` placeholders written as a parameter Postgres's grammar reads, `$0…1`, in the same bytes, so that a tree of it holds every offset of the text. Every other dialect's grammar, or reading, takes them as written.
pub fn for_grammar(dialect: Dialect, text: &str) -> std::borrow::Cow<'_, str> {
    if dialect != Dialect::Postgres {
        return text.into();
    }
    let Ok(lexed) = reylo_engine_core::lex::lex(text, reylo_engine_core::dialect::Dialect::Postgres) else { return text.into() };
    let mut out = text.as_bytes().to_vec();
    let mut changed = false;
    for p in &lexed.placeholders {
        let span = &text.as_bytes()[p.start..p.end];
        if span.first() == Some(&b':') && span.len() >= 2 {
            out[p.start] = b'$';
            out[p.start + 1..p.end].fill(b'0');
            out[p.end - 1] = b'1';
            changed = true;
        }
    }
    match (changed, String::from_utf8(out)) {
        (true, Ok(written)) => written.into(),
        _ => text.into(),
    }
}

/// The tokens of `tree`, spelled as `text` writes them, leaving out a statement end nothing wrote. `tree` is of `text`, or of [`for_grammar`]'s copy of it.
pub fn tokens<'s>(dialect: Dialect, tree: &Tree<'_>, text: &'s str) -> Vec<Tok<'s>> {
    tree.leaves()
        .filter_map(|id| {
            let n = tree.node(id);
            (n.end > n.start).then(|| {
                let text = &text[n.start as usize..n.end as usize];
                Tok { text, start: n.start, end: n.end, class: class_of(dialect, text) }
            })
        })
        .collect()
}

/// What a name in a FROM list stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A table, or a WITH query of that name.
    Table,
    /// A subquery, with the columns it names.
    Derived(Vec<Named>),
    /// A function's rows, whose columns its text doesn't say.
    Function,
}

/// A table reference of one query.
#[derive(Clone, Debug)]
pub struct Source {
    pub schema: Option<String>,
    pub name: String,
    pub key: String,
    pub alias: Option<String>,
    pub alias_key: Option<String>,
    pub at: (u32, u32),
    pub alias_at: Option<(u32, u32)>,
    pub kind: SourceKind,
}

impl Source {
    /// Whether `word`, compared as the dialect compares names, names this source: its alias, or its name where it has none.
    pub fn answers_to(&self, word: &str) -> bool {
        match &self.alias_key {
            Some(alias) => alias == word,
            None => self.kind == SourceKind::Table && self.key == word,
        }
    }
}

/// A WITH query.
#[derive(Clone, Debug)]
pub struct Cte {
    pub name: String,
    pub key: String,
    pub at: (u32, u32),
    pub columns: Vec<Named>,
    /// The tokens of its body.
    pub body: (usize, usize),
    /// Whether its body may name it.
    pub recursive: bool,
}

/// One query of a statement: the statement itself, or a subquery.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub parent: Option<usize>,
    pub sources: Vec<Source>,
    pub ctes: Vec<Cte>,
}

/// The queries of a statement, and which one each of its tokens belongs to.
#[derive(Debug)]
pub struct Scopes {
    pub queries: Vec<Query>,
    pub of_token: Vec<usize>,
}

impl Scopes {
    /// The queries a name at token `at` can see, innermost first.
    pub fn chain(&self, at: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut query = self.of_token.get(at).copied().or(Some(0));
        while let Some(q) = query {
            out.push(q);
            query = self.queries[q].parent;
        }
        out
    }

    /// The source `word` names from token `at`.
    pub fn source(&self, at: usize, word: &str) -> Option<&Source> {
        self.chain(at).into_iter().find_map(|q| self.queries[q].sources.iter().find(|s| s.answers_to(word)))
    }

    /// The WITH query `key` names from token `at`.
    pub fn cte(&self, at: usize, key: &str) -> Option<&Cte> {
        self.chain(at).into_iter().find_map(|q| {
            let list = &self.queries[q].ctes;
            // Inside a WITH query that isn't recursive, only those before it are in scope.
            let limit = list.iter().position(|c| !c.recursive && (c.body.0..c.body.1).contains(&at)).unwrap_or(list.len());
            list[..limit].iter().find(|c| c.key == key)
        })
    }

    /// The source written at this place, by its name or its alias.
    pub fn source_at(&self, place: (u32, u32)) -> Option<&Source> {
        self.queries.iter().flat_map(|q| &q.sources).find(|s| s.at == place || s.alias_at == Some(place))
    }
}

/// Words that follow a table reference, so are no alias of it.
const NOT_AN_ALIAS: &[&str] = &[
    "ARRAY", "AS", "CROSS", "DEFAULT", "DO", "EXCEPT", "FETCH", "FINAL", "FOR", "FORCE", "FORMAT", "FROM", "FULL", "GROUP", "HAVING", "IGNORE", "INNER", "INTERSECT", "JOIN", "LEFT", "LIMIT", "MATCHED", "NATURAL", "NOT", "OFFSET", "ON", "OPTION", "ORDER", "OUTER", "OUTPUT", "OVERRIDING", "PARTITION", "PREWHERE", "QUALIFY", "RETURNING", "RIGHT", "SAMPLE", "SELECT", "SET", "SETTINGS", "TABLESAMPLE", "UNION", "USE", "USING", "VALUES", "WHEN", "WHERE", "WINDOW", "WITH",
];

/// Words that end a FROM list.
const ENDS_FROM: &[&str] = &["WHERE", "GROUP", "HAVING", "ORDER", "LIMIT", "OFFSET", "FETCH", "FOR", "WINDOW", "UNION", "INTERSECT", "EXCEPT", "RETURNING", "SET", "VALUES", "QUALIFY", "PREWHERE", "SETTINGS", "FORMAT", "WHEN"];

fn is_alias(t: &Tok<'_>) -> bool {
    t.class == Class::Quoted || (t.class == Class::Word && !t.is_any(NOT_AN_ALIAS))
}

/// The index after the group that opens at `at`.
fn past_group(toks: &[Tok<'_>], at: usize) -> usize {
    let (mut depth, mut i) = (0, at);
    while let Some(t) = toks.get(i) {
        if t.is_punct("(") {
            depth += 1;
        } else if t.is_punct(")") {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    i
}

/// The names a query's RETURNING or select list gives its columns. An item with no name of its own, a `*` among them, adds none.
fn output_names(dialect: Dialect, toks: &[Tok<'_>]) -> Vec<Named> {
    let Some((from, to)) = returning_list(toks).or_else(|| select_list(toks)) else { return Vec::new() };
    items(toks, from, to).into_iter().filter_map(|(a, b)| item_name(&toks[a..b]).map(|t| named(dialect, t))).collect()
}

/// The token that names a select list item's column: its alias, or the column it reads, cast or not.
fn item_name<'t, 's>(item: &'t [Tok<'s>]) -> Option<&'t Tok<'s>> {
    let usable = |t: &&Tok<'_>| t.is_name() && !t.is_any(RESERVED);
    if let [.., word, alias] = item {
        if word.is("AS") {
            return Some(alias).filter(usable);
        }
    }
    let cast = item.iter().position(|t| t.is_punct("::")).unwrap_or(item.len());
    let after_cast = item.get(cast + 1..).unwrap_or(&[]);
    // A column, cast or not: what follows `::` is a type alone, whose name, parentheses and brackets are all it holds.
    if after_cast.iter().all(|t| t.is_name() || t.class == Class::Number || ["::", ".", "(", ")", "[", "]", ","].iter().any(|p| t.is_punct(p))) {
        // `id::text label`: a name after the type is its alias, unless it's the last word of a type of several.
        if let [.., before, alias] = after_cast {
            if alias.is_name() && (before.is_name() || before.is_punct(")") || before.is_punct("]")) && !alias.is_any(TYPE_ENDINGS) {
                return Some(alias).filter(usable);
            }
        }
        match &item[..cast] {
            [one] => return Some(one).filter(usable),
            [q, dot, one] if q.is_name() && dot.is_punct(".") => return Some(one).filter(usable),
            _ => {}
        }
    }
    // `count(*) total`: a name right after a value is its alias.
    let [.., before, alias] = item else { return None };
    let after_value = before.is_punct(")") || matches!(before.class, Class::Number | Class::String | Class::Quoted) || before.is("END") || (before.class == Class::Word && !before.is_any(RESERVED));
    Some(alias).filter(usable).filter(|_| after_value)
}

/// The words that end a type of several words, as `double precision` does.
const TYPE_ENDINGS: &[&str] = &["VARYING", "PRECISION", "ZONE", "YEAR", "MONTH", "DAY", "HOUR", "MINUTE", "SECOND"];

/// Where the RETURNING list of the statement in `toks` runs, at its own level.
fn returning_list(toks: &[Tok<'_>]) -> Option<(usize, usize)> {
    let mut depth = 0;
    for (i, t) in toks.iter().enumerate() {
        if t.is_punct("(") {
            depth += 1;
        } else if t.is_punct(")") {
            depth -= 1;
        } else if depth == 0 && t.is("RETURNING") {
            return Some((i + 1, toks.len()));
        }
    }
    None
}

/// Where the select list of the query in `toks` runs, at its own level.
pub fn select_list(toks: &[Tok<'_>]) -> Option<(usize, usize)> {
    let mut depth = 0;
    let mut start = None;
    for (i, t) in toks.iter().enumerate() {
        if t.is_punct("(") {
            depth += 1;
        } else if t.is_punct(")") {
            depth -= 1;
        } else if depth == 0 {
            match start {
                None if t.is("SELECT") => {
                    let mut at = i + 1;
                    while toks.get(at).is_some_and(|n| n.is("DISTINCT") || n.is("ALL")) {
                        at += 1;
                    }
                    start = Some(at);
                }
                Some(s) if t.is_any(&["FROM", "WHERE", "GROUP", "HAVING", "ORDER", "LIMIT", "UNION", "INTERSECT", "EXCEPT", "WINDOW", "INTO", "FETCH", "OFFSET"]) => return Some((s, i)),
                _ => {}
            }
        }
    }
    start.map(|s| (s, toks.len()))
}

/// The comma-separated items of `toks[from..to]`, at its own level.
pub fn items(toks: &[Tok<'_>], from: usize, to: usize) -> Vec<(usize, usize)> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0, from);
    for (i, t) in toks.iter().enumerate().take(to).skip(from) {
        if t.is_punct("(") {
            depth += 1;
        } else if t.is_punct(")") {
            depth -= 1;
        } else if depth == 0 && t.is_punct(",") {
            out.push((start, i));
            start = i + 1;
        }
    }
    if start < to {
        out.push((start, to));
    }
    out
}

/// Which column of the statement's result the token at `at` writes, where its select list item is a plain column or an alias, and no `*` before it moves the count.
pub fn output_column(toks: &[Tok<'_>], at: usize) -> Option<usize> {
    let (from, to) = select_list(toks)?;
    let list = items(toks, from, to);
    let k = list.iter().position(|(a, b)| (*a..*b).contains(&at))?;
    if list[..=k].iter().any(|(a, b)| toks[*a..*b].iter().any(|t| t.is_punct("*"))) {
        return None;
    }
    let (a, b) = list[k];
    let item = &toks[a..b];
    let (reference, alias) = match item {
        [.., as_word, alias] if as_word.is("AS") => (&item[..item.len() - 2], Some(alias)),
        _ => (item, None),
    };
    let plain = match reference {
        [one] => one.is_name(),
        [q, dot, one] => q.is_name() && dot.is_punct(".") && one.is_name(),
        _ => false,
    };
    let on_alias = alias.is_some_and(|x| x.start == toks[at].start);
    (on_alias || (plain && reference.last().is_some_and(|x| x.start == toks[at].start))).then_some(k)
}

#[derive(Clone, Copy)]
struct Frame {
    query: usize,
    /// Whether it reads a query's clauses, and isn't a function's arguments.
    reads: bool,
    in_from: bool,
    selected: bool,
}

/// A table reference at `at`, and the index after it.
fn source(dialect: Dialect, toks: &[Tok<'_>], mut at: usize) -> (Option<Source>, usize) {
    while toks.get(at).is_some_and(|t| t.is_any(&["ONLY", "LATERAL", "IF", "NOT", "EXISTS"])) {
        at += 1;
    }
    // `x(a, b)` renames what `x` holds.
    let listed = |i: usize| -> Option<Vec<Named>> {
        toks.get(i).filter(|t| t.is_punct("("))?;
        let past = past_group(toks, i);
        Some(toks[i + 1..past.saturating_sub(1).max(i + 1)].iter().filter(|t| t.is_name()).map(|t| named(dialect, t)).collect())
    };
    let alias_at = |i: usize| -> (Option<Tok<'_>>, usize) {
        match (toks.get(i), toks.get(i + 1)) {
            (Some(a), Some(n)) if a.is("AS") && n.is_name() => (Some(*n), i + 2),
            (Some(a), _) if is_alias(a) => (Some(*a), i + 1),
            _ => (None, i),
        }
    };
    if toks.get(at).is_some_and(|t| t.is_punct("(")) {
        let past = past_group(toks, at);
        if !toks.get(at + 1).is_some_and(|t| t.is_any(&["SELECT", "WITH", "VALUES"])) {
            return (None, at);
        }
        let (alias, after) = alias_at(past);
        let Some(alias) = alias else { return (None, after) };
        let columns = listed(after).unwrap_or_else(|| output_names(dialect, &toks[at + 1..past.saturating_sub(1).max(at + 1)]));
        return (Some(Source { schema: None, name: alias.name(), key: key(dialect, &alias), alias: Some(alias.name()), alias_key: Some(key(dialect, &alias)), at: alias.at(), alias_at: Some(alias.at()), kind: SourceKind::Derived(columns) }), after);
    }
    let Some(first) = toks.get(at).filter(|t| t.is_name()) else { return (None, at) };
    let (mut last, mut schema, mut i) = (*first, None, at + 1);
    while toks.get(i).is_some_and(|t| t.is_punct(".")) && toks.get(i + 1).is_some_and(Tok::is_name) {
        schema = Some(key(dialect, &last));
        last = toks[i + 1];
        i += 2;
    }
    let call = toks.get(i).is_some_and(|t| t.is_punct("("));
    // `create table t (…)` and `insert into t (…)` name a table before their list; in FROM, that is a function.
    let in_from_list = at > 0 && (toks[at - 1].is_any(&["FROM", "JOIN", "LATERAL", "USING"]) || toks[at - 1].is_punct(",") || toks[at - 1].is_punct("("));
    if call && in_from_list {
        let (alias, after) = alias_at(past_group(toks, i));
        let kind = listed(after).filter(|_| alias.is_some()).map_or(SourceKind::Function, SourceKind::Derived);
        return (Some(Source { schema, name: last.name(), key: key(dialect, &last), alias: alias.as_ref().map(Tok::name), alias_key: alias.as_ref().map(|a| key(dialect, a)), at: last.at(), alias_at: alias.as_ref().map(Tok::at), kind }), after);
    }
    let (alias, after) = if call { (None, i) } else { alias_at(i) };
    (Some(Source { schema, name: last.name(), key: key(dialect, &last), alias: alias.as_ref().map(Tok::name), alias_key: alias.as_ref().map(|a| key(dialect, a)), at: last.at(), alias_at: alias.as_ref().map(Tok::at), kind: SourceKind::Table }), after)
}

/// The queries of one statement's tokens.
pub fn scopes(dialect: Dialect, toks: &[Tok<'_>]) -> Scopes {
    let mut queries = vec![Query::default()];
    let mut of_token = Vec::with_capacity(toks.len());
    let mut frames = vec![Frame { query: 0, reads: true, in_from: false, selected: false }];
    let creates = toks.first().is_some_and(|t| t.is("CREATE"));
    for i in 0..toks.len() {
        let t = toks[i];
        let top = *frames.last().expect("the statement's own frame");
        if t.is_punct("(") {
            of_token.push(top.query);
            let next = toks.get(i + 1);
            if next.is_some_and(|n| n.is_any(&["SELECT", "WITH", "VALUES"])) {
                queries.push(Query { parent: Some(top.query), ..Query::default() });
                frames.push(Frame { query: queries.len() - 1, reads: true, in_from: false, selected: false });
            } else if top.reads && top.in_from && i > 0 && (toks[i - 1].is_any(&["FROM", "JOIN"]) || toks[i - 1].is_punct(",") || toks[i - 1].is_punct("(")) {
                // `from (a join b on …)`: the same query's FROM list, in parentheses.
                let (found, _) = source(dialect, toks, i + 1);
                queries[top.query].sources.extend(found);
                frames.push(Frame { query: top.query, reads: true, in_from: true, selected: true });
            } else {
                frames.push(Frame { reads: false, in_from: false, ..top });
            }
            continue;
        }
        if t.is_punct(")") {
            if frames.len() > 1 {
                frames.pop();
            }
            // A closing parenthesis stands in the query around the group, as the opening one does.
            of_token.push(frames.last().expect("the statement's own frame").query);
            continue;
        }
        of_token.push(top.query);
        if !top.reads {
            continue;
        }
        let frame = frames.last_mut().expect("the statement's own frame");
        if t.is_punct(",") {
            if frame.in_from {
                let (found, _) = source(dialect, toks, i + 1);
                queries[frame.query].sources.extend(found);
            }
            continue;
        }
        if t.class != Class::Word {
            continue;
        }
        let before = i.checked_sub(1).map(|p| toks[p]);
        // The second part of `t.from` is a column, unless the name after the dot is still being typed and what follows it is the clause.
        if before.is_some_and(|b| b.is_punct(".")) && !(t.is("FROM") && toks.get(i + 1).is_some_and(|n| n.is_punct("(") || (n.is_name() && !n.is("FROM")))) {
            continue;
        }
        if t.is("WITH") {
            let query = frame.query;
            read_ctes(dialect, toks, i, &mut queries[query]);
        } else if t.is("FROM") {
            // `a is distinct from b` compares.
            let compares = before.is_some_and(|b| b.is("DISTINCT")) && i >= 2 && toks[i - 2].is_any(&["IS", "NOT"]);
            if !compares {
                frame.in_from = true;
                let (found, _) = source(dialect, toks, i + 1);
                queries[frame.query].sources.extend(found);
            }
        } else if t.is("JOIN") {
            frame.in_from = true;
            let (found, _) = source(dialect, toks, i + 1);
            queries[frame.query].sources.extend(found);
        } else if t.is("USING") {
            if !toks.get(i + 1).is_some_and(|n| n.is_punct("(")) {
                frame.in_from = true;
                let (found, _) = source(dialect, toks, i + 1);
                queries[frame.query].sources.extend(found);
            }
        } else if t.is("SELECT") {
            frame.selected = true;
            frame.in_from = false;
        } else if t.is("UPDATE") {
            frame.in_from = false;
            if !before.is_some_and(|b| b.is_any(&["FOR", "DO", "ON", "KEY", "NO", "THEN"])) {
                let (found, _) = source(dialect, toks, i + 1);
                queries[frame.query].sources.extend(found);
            }
        } else if t.is_any(&["UNION", "INTERSECT", "EXCEPT"]) && !before.is_some_and(|b| b.is_punct("*")) {
            // Each side of a set operation has a FROM list of its own and sees the same WITH queries.
            let (parent, ctes) = (queries[frame.query].parent, queries[frame.query].ctes.clone());
            queries.push(Query { parent, sources: Vec::new(), ctes });
            frame.query = queries.len() - 1;
            frame.in_from = false;
            frame.selected = false;
        } else if t.is_any(ENDS_FROM) {
            frame.in_from = false;
        } else if t.is("INTO") {
            if !frame.selected {
                let (found, _) = source(dialect, toks, i + 1);
                queries[frame.query].sources.extend(found);
            }
        } else if t.is("TABLE") || t.is("REFERENCES") || (t.is("ON") && creates && !frame.in_from && !frame.selected) {
            let (found, _) = source(dialect, toks, i + 1);
            queries[frame.query].sources.extend(found);
        }
    }
    Scopes { queries, of_token }
}

fn read_ctes(dialect: Dialect, toks: &[Tok<'_>], with: usize, query: &mut Query) {
    let mut j = with + 1;
    let recursive = toks.get(j).is_some_and(|t| t.is("RECURSIVE"));
    if recursive {
        j += 1;
    }
    while let Some(name) = toks.get(j).filter(|t| t.is_name()) {
        let mut k = j + 1;
        let mut listed: Option<Vec<Named>> = None;
        if toks.get(k).is_some_and(|t| t.is_punct("(")) {
            let past = past_group(toks, k);
            listed = Some(toks[k + 1..past.saturating_sub(1).max(k + 1)].iter().filter(|t| t.is_name()).map(|t| named(dialect, t)).collect());
            k = past;
        }
        if !toks.get(k).is_some_and(|t| t.is("AS")) {
            break;
        }
        k += 1;
        while toks.get(k).is_some_and(|t| t.is("NOT") || t.is("MATERIALIZED")) {
            k += 1;
        }
        if !toks.get(k).is_some_and(|t| t.is_punct("(")) {
            break;
        }
        let past = past_group(toks, k);
        let columns = listed.unwrap_or_else(|| output_names(dialect, &toks[k + 1..past.saturating_sub(1).max(k + 1)]));
        query.ctes.push(Cte { name: name.name(), key: key(dialect, name), at: name.at(), columns, body: (k, past), recursive });
        if !toks.get(past).is_some_and(|t| t.is_punct(",")) {
            break;
        }
        j = past + 1;
    }
}

/// A table a project's SQL file creates.
#[derive(Clone, Debug)]
pub struct Created {
    pub schema: Option<String>,
    pub key: String,
    pub at: (u32, u32),
    pub columns: Vec<(String, (u32, u32))>,
}

/// Words that open a table constraint, so name no column.
const NOT_A_COLUMN: &[&str] = &["CHECK", "CONSTRAINT", "EXCLUDE", "FOREIGN", "FULLTEXT", "INDEX", "KEY", "LIKE", "PERIOD", "PRIMARY", "SPATIAL", "UNIQUE"];

/// Each `CREATE TABLE` in `toks`, with its columns, each column keyed as `dialect` compares names.
pub fn created_tables(dialect: Dialect, toks: &[Tok<'_>]) -> Vec<Created> {
    let mut out = Vec::new();
    for i in 0..toks.len() {
        if !toks[i].is("CREATE") {
            continue;
        }
        let mut j = i + 1;
        while toks.get(j).is_some_and(|t| t.is_any(&["OR", "REPLACE", "TEMP", "TEMPORARY", "UNLOGGED", "GLOBAL", "LOCAL", "FOREIGN"])) {
            j += 1;
        }
        if !toks.get(j).is_some_and(|t| t.is("TABLE")) {
            continue;
        }
        let (Some(table), after) = source(dialect, toks, j + 1) else { continue };
        let mut columns = Vec::new();
        if toks.get(after).is_some_and(|t| t.is_punct("(")) {
            let (mut depth, mut k, mut starts) = (0, after, false);
            while let Some(t) = toks.get(k) {
                if t.is_punct("(") {
                    depth += 1;
                    starts = depth == 1;
                } else if t.is_punct(")") {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                } else if depth == 1 && t.is_punct(",") {
                    starts = true;
                } else {
                    if starts && t.is_name() && !(t.class == Class::Word && t.is_any(NOT_A_COLUMN)) {
                        columns.push((key(dialect, t), t.at()));
                    }
                    starts = false;
                }
                k += 1;
            }
        }
        out.push(Created { schema: table.schema, key: table.key, at: table.at, columns });
    }
    out
}

/// Where each line of a text starts, for the protocol's positions. A line ends at `\n`, `\r\n` or a lone `\r`.
pub struct Lines {
    starts: Vec<u32>,
}

impl Lines {
    pub fn new(text: &str) -> Lines {
        let b = text.as_bytes();
        let mut starts = vec![0];
        for (i, c) in b.iter().enumerate() {
            if *c == b'\n' || (*c == b'\r' && b.get(i + 1) != Some(&b'\n')) {
                starts.push(i as u32 + 1);
            }
        }
        Lines { starts }
    }

    /// A position, in UTF-16 units, as the byte offset it names; one past a line's end is the line's end.
    pub fn offset(&self, text: &str, line: u32, character: u32) -> usize {
        let Some(&start) = self.starts.get(line as usize) else { return text.len() };
        let start = start as usize;
        let mut units = 0;
        for (i, c) in text[start..].char_indices() {
            if units >= character || c == '\n' || c == '\r' {
                return start + i;
            }
            units += c.len_utf16() as u32;
        }
        text.len()
    }

    /// A byte offset as the protocol's line and character.
    pub fn position(&self, text: &str, offset: usize) -> (u32, u32) {
        let mut end = offset.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let line = self.starts.partition_point(|s| *s as usize <= end) - 1;
        let start = self.starts[line] as usize;
        (line as u32, text[start..end].chars().map(|c| c.len_utf16() as u32).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dialect: Dialect, sql: &str) -> Scopes {
        let tree = syntax::parse(dialect, sql);
        scopes(dialect, &tokens(dialect, &tree, sql))
    }

    fn sources(s: &Scopes, query: usize) -> Vec<(&str, Option<&str>)> {
        s.queries[query].sources.iter().map(|x| (x.name.as_str(), x.alias.as_deref())).collect()
    }

    #[test]
    fn each_side_of_a_set_operation_reads_its_own_from_list() {
        let s = read(Dialect::Postgres, "with c as (select 1) select u.id from users u union all select u.body from posts u join c on true");
        let last = s.of_token.len() - 1;
        assert_eq!(s.source(last, "u").map(|x| x.name.as_str()), Some("posts"));
        assert_eq!(s.source(12, "u").map(|x| x.name.as_str()), Some("users"));
        assert!(s.cte(last, "c").is_some());
    }

    #[test]
    fn a_cast_keeps_its_alias_and_a_type_of_two_words_is_none() {
        let s = read(Dialect::Postgres, "with x as (select id::text label, name::text, a::double precision, b::int::text, c::numeric(3) d, price::numeric / 100 dollars, e::int + f from t) select 1 from x");
        let names: Vec<&str> = s.queries.iter().flat_map(|q| &q.ctes).flat_map(|c| &c.columns).map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["label", "name", "a", "b", "d", "dollars"]);
    }

    #[test]
    fn reads_tables_and_aliases_in_every_dialect() {
        for dialect in [Dialect::Postgres, Dialect::Sqlite, Dialect::Mysql, Dialect::ClickHouse, Dialect::SqlServer] {
            let s = read(dialect, "select u.id from app.users u, tags join posts as p on p.user_id = u.id where u.id = 1");
            assert_eq!(sources(&s, 0), [("users", Some("u")), ("tags", None), ("posts", Some("p"))], "{dialect:?}");
            assert_eq!(s.queries[0].sources[0].schema.as_deref(), Some("app"));
        }
    }

    #[test]
    fn keeps_each_subquery_to_itself() {
        let s = read(Dialect::Postgres, "select u.id from users u where exists (select 1 from posts u where u.id = 1)");
        assert_eq!(sources(&s, 0), [("users", Some("u"))]);
        assert_eq!(sources(&s, 1), [("posts", Some("u"))]);
    }

    #[test]
    fn reads_every_kind_of_from_item() {
        let s = read(Dialect::Postgres, "select 1 from users u, (select id, name as n from t) x, generate_series(1, 3) g, posts p");
        let names: Vec<_> = s.queries[0].sources.iter().map(|x| x.alias.as_deref().unwrap()).collect();
        assert_eq!(names, ["u", "x", "g", "p"]);
        let SourceKind::Derived(columns) = &s.queries[0].sources[1].kind else { panic!("a subquery") };
        assert_eq!(columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "n"]);
        assert_eq!(s.queries[0].sources[2].kind, SourceKind::Function);
        assert_eq!(sources(&read(Dialect::Postgres, "delete from users a using posts b where b.id = 1"), 0), [("users", Some("a")), ("posts", Some("b"))]);
        assert_eq!(sources(&read(Dialect::Postgres, "merge into users t using posts s on s.id = t.id when matched then delete"), 0), [("users", Some("t")), ("posts", Some("s"))]);
    }

    #[test]
    fn a_from_that_is_no_from_clause_names_nothing() {
        for sql in ["select a is distinct from status from users", "select extract(epoch from status) from users", "select 1 from users for update skip locked", "insert into users values (1) on conflict (id) do update set name = 1"] {
            let s = read(Dialect::Postgres, sql);
            assert_eq!(sources(&s, 0), [("users", None)], "{sql}");
        }
    }

    #[test]
    fn reads_with_queries_and_their_columns() {
        let s = read(Dialect::Postgres, "with recursive a(x) as (select 1), b as not materialized (select id, 2 as two from a) select * from b");
        let ctes: Vec<_> = s.queries[0].ctes.iter().map(|c| (c.name.as_str(), c.columns.iter().map(|x| x.name.as_str()).collect::<Vec<_>>())).collect();
        assert_eq!(ctes, [("a", vec!["x"]), ("b", vec!["id", "two"])]);
    }

    #[test]
    fn names_a_subquerys_columns_as_the_database_does() {
        let columns = |sql: &str| {
            let s = read(Dialect::Postgres, sql);
            s.queries[0].ctes[0].columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(columns("with x as (select id::text, a + b, count(*) n, t.c from t) select 1"), ["id", "n", "c"]);
        let s = read(Dialect::Postgres, "select 1 from (select 1 as p, 2 as q) x(a, b)");
        let SourceKind::Derived(listed) = &s.queries[0].sources[0].kind else { panic!("a subquery") };
        assert_eq!(listed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn a_with_query_that_isnt_recursive_doesnt_see_itself() {
        let sql = "with users as (select id from users) select * from users";
        let tree = syntax::parse(Dialect::Postgres, sql);
        let toks = tokens(Dialect::Postgres, &tree, sql);
        let s = scopes(Dialect::Postgres, &toks);
        let inner = toks.iter().enumerate().filter(|(_, t)| t.text == "users").map(|(i, _)| i).nth(1).unwrap();
        let outer = toks.iter().rposition(|t| t.text == "users").unwrap();
        assert!(s.cte(inner, "users").is_none());
        assert!(s.cte(outer, "users").is_some());
    }

    #[test]
    fn folds_names_as_the_dialect_does() {
        let sql = "\"USERS\" USERS";
        let tree = syntax::parse(Dialect::Postgres, sql);
        let toks = tokens(Dialect::Postgres, &tree, sql);
        assert_eq!((key(Dialect::Postgres, &toks[0]), key(Dialect::Postgres, &toks[1])), ("USERS".to_string(), "users".to_string()));
    }

    #[test]
    fn a_quoted_name_still_being_typed_reads_whole() {
        for (text, name) in [("\"é", "é"), ("\"users", "users"), ("\"a\"\"b\"", "a\"b"), ("\"", "")] {
            assert_eq!(Tok { text, start: 0, end: text.len() as u32, class: Class::Quoted }.name(), name);
        }
    }

    #[test]
    fn finds_the_result_column_a_token_writes() {
        let sql = "select u.name as id, u.id as other, p.body, 1 + x from users u";
        let tree = syntax::parse(Dialect::Postgres, sql);
        let toks = tokens(Dialect::Postgres, &tree, sql);
        let at = |w: &str| toks.iter().position(|t| t.text == w).unwrap();
        assert_eq!(output_column(&toks, at("other")), Some(1));
        assert_eq!(output_column(&toks, at("body")), Some(2));
        assert_eq!(output_column(&toks, at("x")), None);
        assert_eq!(output_column(&toks, at("users")), None);
    }

    #[test]
    fn writes_placeholders_for_the_grammar_in_the_same_bytes() {
        let sql = "select :id, :name::text, ':no', a::int";
        let written = for_grammar(Dialect::Postgres, sql);
        assert_eq!(written, "select $01, $0001::text, ':no', a::int");
        assert!(syntax::parse(Dialect::Postgres, &written).errors.is_empty());
        assert_eq!(for_grammar(Dialect::Sqlite, sql), sql);
    }

    #[test]
    fn reads_what_a_file_creates() {
        let sql = "create table if not exists public.users (id int primary key, \"full name\" text, age numeric(3, 1), constraint k unique (age), primary key (id));";
        let tree = syntax::parse(Dialect::Postgres, sql);
        let made = created_tables(Dialect::Postgres, &tokens(Dialect::Postgres, &tree, sql));
        assert_eq!((made[0].key.as_str(), made[0].schema.as_deref()), ("users", Some("public")));
        assert_eq!(made[0].columns.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["id", "full name", "age"]);
    }

    #[test]
    fn positions_count_utf16_and_every_line_ending() {
        let text = "a\nsé😀x\r\nb\rc";
        let lines = Lines::new(text);
        assert_eq!(lines.offset(text, 1, 4), text.find('😀').unwrap() + 4);
        assert_eq!(lines.position(text, text.find('x').unwrap()), (1, 4));
        assert_eq!(lines.position(text, text.find('b').unwrap()), (2, 0));
        assert_eq!(lines.position(text, text.find('c').unwrap()), (3, 0));
        assert_eq!(lines.offset(text, 9, 0), text.len());
        assert_eq!(lines.offset(text, 1, 99), text.find('\r').unwrap());
    }
}
