//! Which MySQL parameters the caller may pass null for, by the rule of Postgres and SQLite
//! (`postgres::nullable_params`), read off the statement's syntax tree, since MySQL's prepare says nothing
//! of a parameter's nullability: one cast to `nullable(…)`, and one that is, bare, with no cast,
//! the whole value written to a column that may hold null (a row of an INSERT's or a REPLACE's
//! VALUES, the SELECT that feeds one, its SET list, an UPDATE's SET, ON DUPLICATE KEY UPDATE),
//! unless it also fills a column that refuses null: by its NOT NULL, by an enforced CHECK that is
//! false wherever it is NULL, or by a NOT NULL generated column that is NULL wherever it is. Any
//! other cast parameter is its cast's type, which the cast states whole.
//! Every other parameter is a non-null value, one the statement coalesces, tests for null or
//! compares with `<=>` included.

use crate::fresh::Tables;
use crate::host::{as_str, Host, Lazy};
use crate::lex;
use crate::mysql::by_table;
use crate::mysql_keys::Cte;
use crate::mysql_names::Reads;
use reylo_engine_core::refusal::{self, Cmp, Probe};
use syntax::mysql::ast::{self, Expr, FunctionArg, FunctionArgExpr, Query, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins};
use std::collections::{HashMap, HashSet};

/// Each column of the named tables of the current schema, whether it may hold NULL, its `extra`,
/// and a generated column's expression, as `information_schema` prints it ([`refused_by`]), in the
/// order an INSERT without a column list fills them, which passes over an INVISIBLE column. A
/// view's column is taken to refuse NULL: what a write through it takes is its table's to say. `?`
/// is one table's name ([`by_table`]).
const COLUMNS_QUERY: &str = "select c.table_name, c.column_name, if(t.table_type = 'VIEW', 'NO', c.is_nullable), c.extra, c.generation_expression from information_schema.columns c \
    join information_schema.tables t on t.table_schema = c.table_schema and t.table_name = c.table_name \
    where c.table_schema = database() and c.table_name = ? order by c.table_name, c.ordinal_position";

/// The enforced CHECKs of the named tables of the current schema, as the server prints each
/// (MySQL 8.0.16 and later; a server without the views answers an error, and no CHECK refuses
/// null there).
const CHECKS_QUERY: &str = "select tc.table_name, cc.check_clause from information_schema.table_constraints tc \
    join information_schema.check_constraints cc on cc.constraint_schema = tc.constraint_schema and cc.constraint_name = tc.constraint_name \
    where tc.table_schema = database() and tc.constraint_type = 'CHECK' and tc.enforced = 'YES' and tc.table_name = ?";

/// Each column's type of the named tables of the current schema, for [`typed`]: its
/// `data_type`, its `column_type` (`tinyint(1)`, `bit(1)`, `bigint unsigned`), its `extra`, its
/// comment, empty for none (a view's column has its table column's), and its character set, NULL
/// for a column that is no text. Each table is named as a column's origin in the prepare names it,
/// which with `lower_case_table_names` 2 (macOS) is in lower case, while the catalog keeps the case
/// the table was made with.
pub(crate) const TYPES_QUERY: &str = "select if(@@lower_case_table_names = 2, lower(c.table_name), c.table_name), c.column_name, c.data_type, c.column_type, c.extra, c.column_comment, c.character_set_name from information_schema.columns c \
    where c.table_schema = database() and c.table_name = ? order by c.table_name, c.ordinal_position";

/// The parameter each `?` of the rewritten text is, by where it stands as the parser counts:
/// lines and characters from 1.
pub(crate) type Names = HashMap<(u64, u64), String>;

pub(crate) fn names(rewritten: &lex::Rewritten) -> Names {
    let mut out = Names::new();
    for (i, &(_, _, start, _)) in rewritten.spans.iter().enumerate() {
        let before = &rewritten.sql[..start.min(rewritten.sql.len())];
        let line = before.matches('\n').count() as u64 + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) as u64 + 1;
        if let Some(name) = rewritten.order.get(i) {
            out.insert((line, column), name.clone());
        }
    }
    out
}

/// Asks for the columns of the tables the statement writes a parameter to or compares one with,
/// so that the answer comes in the round of the statement's own prepare.
pub(crate) fn prefetch(statement: &Statement, names: &Names, host: &Host) {
    let tables = written(statement, names);
    if !tables.is_empty() {
        let _ = by_table(host, COLUMNS_QUERY, &tables);
        let _ = by_table(host, CHECKS_QUERY, &tables);
    }
    let typed = typed_tables(statement, names);
    if !typed.is_empty() {
        let _ = by_table(host, TYPES_QUERY, &typed);
    }
}

/// The tables whose columns [`typed`] reads: those a parameter is written to or compared with,
/// and those whose columns the result columns read ([`traced_from`]).
fn typed_tables(statement: &Statement, names: &Names) -> Vec<String> {
    let mut tables = written_tables(&typed_targets(statement, names));
    tables.extend(written_tables(&filled(statement, names, Fill::Bare)));
    tables.extend(traced_from(statement).unwrap_or_default());
    tables.sort();
    tables.dedup();
    tables
}

/// The tables of a SELECT whose result columns' origins in the prepare are the columns
/// themselves: one with no WITH and no set operation, over tables (or views) of the current schema
/// alone. Through a derived table or a WITH query MySQL gives the base table and the derived
/// column's own name as a column's origin (`select doc from (select other as doc from o) s` reads
/// `o.doc`), which may be another column of that table; an expression has no origin.
pub(crate) fn traced_from(statement: &Statement) -> Option<Vec<String>> {
    fn factors(factor: &TableFactor, out: &mut Vec<String>) -> Option<()> {
        match factor {
            TableFactor::Table { name, args: None, .. } => out.push(table_name(name)?),
            TableFactor::NestedJoin { table_with_joins, .. } => {
                factors(&table_with_joins.relation, out)?;
                for join in &table_with_joins.joins {
                    factors(&join.relation, out)?;
                }
            }
            _ => return None,
        }
        Some(())
    }
    let Statement::Query(q) = statement else { return None };
    if q.with.is_some() {
        return None;
    }
    let SetExpr::Select(select) = q.body.as_ref() else { return None };
    let mut tables = Vec::new();
    for from in &select.from {
        factors(&from.relation, &mut tables)?;
        for join in &from.joins {
            factors(&join.relation, &mut tables)?;
        }
    }
    Some(tables)
}

/// The parameters written whole to a column or compared whole with one, each with that column.
fn typed_targets(statement: &Statement, names: &Names) -> Vec<(String, String, Target)> {
    let mut targets = filled(statement, names, Fill::Typed);
    targets.extend(compared(statement, names));
    targets
}

/// What the columns the statement names say of its parameters and its result columns ([`typed`]).
#[derive(Debug, Default)]
pub(crate) struct Typed {
    /// Per parameter, the type its columns give it where it differs from the prepare's.
    pub types: HashMap<String, crate::types::Type>,
    /// Per parameter, the comment of the columns it is written to or compared with, where they
    /// share one.
    pub comments: HashMap<String, String>,
    /// Per column of the tables the result columns read ([`traced_from`]), by table as the server
    /// names it and column in lower case, its comment where it has one.
    pub columns: HashMap<(String, String), String>,
    /// Per `ENUM` and `SET` column of those tables, by table as the server names it and column in
    /// lower case, its type with the labels it declares, which the prepare does not give.
    pub labelled: HashMap<(String, String), crate::types::Type>,
    /// The character set each string column of those tables stores its values in, by table as the
    /// server names it and column in lower case ([`crate::mysql_types::character_set`]).
    pub charsets: HashMap<(String, String), String>,
    /// Per parameter written bare and whole to one column, by table as the server names it and
    /// column in lower case, that column: the parameter takes its definition.
    pub written: HashMap<String, (String, String)>,
}

/// The type of each parameter whose column the statement names directly, where it differs from
/// what MySQL's prepare says: the server describes every integer context as a BIGINT and a BIT
/// context as BIT(64), whatever the column, so a parameter written to or compared with a BIGINT
/// column is `int8`, one with a TINYINT(1) or BIT(1) column, which a row returns as a truth
/// value, `bool|int4`, and one with a wider BIT column `bytea`. A row's value then goes back in
/// as a parameter of its type (the round-trip rule). A parameter whose
/// columns disagree keeps the prepare's type. And the comments of those columns and of the columns
/// the result columns read, for their doc comments.
pub(crate) fn typed(statement: &Statement, names: &Names, host: &Host) -> Lazy<Typed> {
    let targets = typed_targets(statement, names);
    let tables = typed_tables(statement, names);
    if tables.is_empty() {
        return Some(Typed::default());
    }
    let (rows, answers) = match crate::mysql::answered_by_table(host, TYPES_QUERY, &tables)? {
        Ok((rows, answers)) => (Ok(rows), answers),
        Err(e) => (Err(e), Default::default()),
    };
    let mut out = Typed::default();
    // Per table as the server names it: each column in lower case, its type, whether it is INVISIBLE, and its comment.
    let mut columns: HashMap<String, Vec<(String, Option<crate::types::Type>, bool, Option<String>)>> = HashMap::new();
    for row in rows.unwrap_or_default() {
        let (Some(table), Some(column), Some(data_type), Some(column_type)) = (row.first().and_then(as_str), row.get(1).and_then(as_str), row.get(2).and_then(as_str), row.get(3).and_then(as_str)) else { continue };
        let invisible = row.get(4).and_then(as_str).is_some_and(|extra| extra.to_ascii_uppercase().contains("INVISIBLE"));
        // An ENUM's and a SET's labels keep their case.
        let labels = crate::mysql_types::labels(column_type);
        let column_type = column_type.to_ascii_lowercase();
        // The column's own type: a BIGINT's is 64 bits, a TINYINT(1)'s the integer a client reads
        // as a truth value, a BIT's its width in bits, an ENUM's one of its labels, and a SET's its
        // members, each one of its labels, which the prepare types as text.
        let unsigned = if column_type.contains("unsigned") { 32 } else { 0 };
        let width = column_type.split_once('(').and_then(|(_, rest)| rest.split(')').next()).and_then(|n| n.trim().parse::<u32>().ok());
        let ty = match data_type.to_ascii_lowercase().as_str() {
            "bigint" => Some(crate::mysql_types::of(8, unsigned, 0, None)),
            "tinyint" if column_type.starts_with("tinyint(1)") => Some(crate::mysql_types::of(1, unsigned, 0, Some(1))),
            "bit" => Some(crate::mysql_types::bits(width)),
            "enum" => Some(crate::mysql_types::enumeration(labels)),
            "set" => Some(crate::mysql_types::set_of(labels)),
            _ => None,
        };
        if matches!(data_type.to_ascii_lowercase().as_str(), "enum" | "set") {
            if let Some(ty) = &ty {
                out.labelled.insert((table.to_string(), column.to_ascii_lowercase()), ty.clone());
            }
        }
        let comment = row.get(5).and_then(as_str).filter(|c| !c.is_empty()).map(String::from);
        if let Some(charset) = row.get(6).and_then(as_str).filter(|c| !c.is_empty()) {
            out.charsets.insert((table.to_string(), column.to_ascii_lowercase()), charset.to_string());
        }
        if let Some(comment) = &comment {
            out.columns.insert((table.to_string(), column.to_ascii_lowercase()), comment.clone());
        }
        columns.entry(table.to_string()).or_default().push((column.to_ascii_lowercase(), ty, invisible, comment));
    }
    // The server's name for a table the statement names in another case ([`crate::mysql_names::Answers`]).
    let server_name = |table: &str| answers.name(table).map(String::from);
    let mut found: HashMap<String, Option<crate::types::Type>> = HashMap::new();
    let mut noted: HashMap<String, Option<String>> = HashMap::new();
    for (param, table, target) in targets {
        let known = columns.get(&table).or_else(|| columns.get(&server_name(&table)?));
        let column = known.and_then(|known| match &target {
            Target::Name(name) => known.iter().find(|(c, _, _, _)| c == name),
            Target::Position(i) => known.iter().filter(|(_, _, invisible, _)| !invisible).nth(*i),
        });
        // A column nothing narrows, or one the statement names that the table lacks, keeps the prepare's type.
        let ty = column.and_then(|(_, ty, _, _)| ty.clone());
        let comment = column.and_then(|(_, _, _, comment)| comment.clone());
        found.entry(param.clone()).and_modify(|seen| if *seen != ty { *seen = None }).or_insert(ty);
        noted.entry(param).and_modify(|seen| if *seen != comment { *seen = None }).or_insert(comment);
    }
    // The one column each bare parameter is written to, the SELECT that feeds an INSERT included.
    let mut written: HashMap<String, Option<(String, String)>> = HashMap::new();
    for (param, table, target) in filled(statement, names, Fill::Bare) {
        let known = columns.get_key_value(&table).or_else(|| columns.get_key_value(&server_name(&table)?));
        let place = known.and_then(|(name, known)| {
            let column = match &target {
                Target::Name(column) => known.iter().find(|(c, _, _, _)| c == column),
                Target::Position(i) => known.iter().filter(|(_, _, invisible, _)| !invisible).nth(*i),
            };
            // A column the rule above narrows keeps that rule, whatever else the parameter is compared with.
            column.filter(|(_, ty, _, _)| ty.is_none()).map(|(column, _, _, _)| (name.clone(), column.clone()))
        });
        written.entry(param).and_modify(|seen| if *seen != place { *seen = None }).or_insert(place);
    }
    out.written = written.into_iter().filter_map(|(param, place)| place.map(|place| (param, place))).collect();
    out.types = found.into_iter().filter_map(|(param, ty)| ty.map(|ty| (param, ty))).collect();
    out.comments = noted.into_iter().filter_map(|(param, comment)| comment.map(|comment| (param, comment))).collect();
    Some(out)
}

/// Every parameter compared whole with a column of the table the statement reads, in its WHERE
/// or its HAVING (`col = :p`, `<>`, `<`, `<=`, `>`, `>=`, `<=>`, `col in (:a, :b)`, `col between
/// :a and :b`): (parameter, table as the statement names it, column). A bare column is the one
/// table's where the statement reads one; a qualified one is the table its name or alias names.
fn compared(statement: &Statement, names: &Names) -> Vec<(String, String, Target)> {
    let mut out = Vec::new();
    match statement {
        Statement::Query(q) => compared_query(q, &[], false, names, &mut out),
        Statement::Update(update) => compared_select(&[&update.table], update.selection.iter().collect(), None, &[], false, names, &mut out),
        Statement::Delete(delete) => {
            let (ast::FromTable::WithFromKeyword(from) | ast::FromTable::WithoutKeyword(from)) = &delete.from;
            compared_select(&from.iter().collect::<Vec<_>>(), delete.selection.iter().collect(), None, &[], false, names, &mut out);
        }
        _ => {}
    }
    out
}

/// The comparisons of a query: its body's, and those inside each of its WITH queries, each read
/// with the WITH queries it sees ([`crate::mysql_keys::ctes_of`]). `enclosed` where it sees the
/// FROM items of an enclosing query, as a LATERAL derived table does.
fn compared_query<'a>(q: &'a Query, outer: &[Cte<'a>], enclosed: bool, names: &Names, out: &mut Vec<(String, String, Target)>) {
    let ctes = crate::mysql_keys::ctes_of(q, outer);
    for (_, body, sees, _) in ctes.iter().skip(outer.len()) {
        if let Some(body) = body {
            compared_query(body, &ctes[..*sees], enclosed, names, out);
        }
    }
    compared_body(&q.body, &ctes, enclosed, names, out);
}

fn compared_body<'a>(body: &'a SetExpr, ctes: &[Cte<'a>], enclosed: bool, names: &Names, out: &mut Vec<(String, String, Target)>) {
    match body {
        SetExpr::Select(s) => {
            // In a HAVING the server reads a bare name as the select list's alias before the FROM's
            // column of that name: an alias of anything but that column itself names no column.
            let aliases: Vec<String> = s
                .projection
                .iter()
                .filter_map(|item| match item {
                    SelectItem::ExprWithAlias { expr, alias } => {
                        let itself = match expr {
                            Expr::Identifier(i) => i.value.eq_ignore_ascii_case(&alias.value),
                            Expr::CompoundIdentifier(parts) => parts.last().is_some_and(|p| p.value.eq_ignore_ascii_case(&alias.value)),
                            _ => false,
                        };
                        (!itself).then(|| alias.value.to_ascii_lowercase())
                    }
                    _ => None,
                })
                .collect();
            let having = s.having.as_ref().map(|having| (having, aliases));
            compared_select(&s.from.iter().collect::<Vec<_>>(), s.selection.iter().collect(), having, ctes, enclosed, names, out)
        }
        SetExpr::SetOperation { left, right, .. } => {
            compared_body(left, ctes, enclosed, names, out);
            compared_body(right, ctes, enclosed, names, out);
        }
        SetExpr::Query(q) => compared_query(q, ctes, enclosed, names, out),
        _ => {}
    }
}

/// The comparisons of one SELECT (or an UPDATE's or a DELETE's WHERE) over its FROM, in its WHERE,
/// its HAVING and its joins' ON clauses, and those inside its derived tables. A column of a
/// derived table or a WITH query is read through to each table column it passes through unchanged
/// ([`crate::mysql_keys::output_origins`]): one, or one per side of a set operation, whose types
/// [`typed`] keeps only where they agree. Beside the HAVING, the select list's aliases, which a
/// bare name there reads first.
fn compared_select<'a>(from: &[&'a TableWithJoins], filters: Vec<&'a Expr>, having: Option<(&'a Expr, Vec<String>)>, ctes: &[Cte<'a>], enclosed: bool, names: &Names, out: &mut Vec<(String, String, Target)>) {
    // The tables by the name the statement reads each by; `plain` where every item is a table.
    let mut tables: Vec<(String, String)> = Vec::new();
    let mut derived: Vec<(String, &Query, &[Cte], Vec<String>)> = Vec::new();
    // The names of the items neither list follows, so that a qualifier one of them may take is
    // taken for no other item's ([`crate::mysql_names::item`]).
    let mut others: Vec<String> = Vec::new();
    let mut plain = true;
    let mut items = 0;
    let listed = |alias: &Option<ast::TableAlias>| -> Vec<String> { alias.as_ref().map(|a| a.columns.iter().map(|c| c.name.value.to_ascii_lowercase()).collect()).unwrap_or_default() };
    for twj in from {
        for factor in std::iter::once(&twj.relation).chain(twj.joins.iter().map(|j| &j.relation)) {
            items += 1;
            match factor {
                TableFactor::Table { name, alias, args: None, .. } => {
                    // A name one of the statement's WITH queries has is that query, never a table.
                    let cte = match name.0.as_slice() {
                        [ast::ObjectNamePart::Identifier(n)] => crate::mysql_names::with_query(ctes.iter().map(|(c, _, _, _)| c.as_str()), &n.value),
                        _ => Reads::Table,
                    };
                    match (cte, table_name(name)) {
                        (Reads::Unknown, _) => {
                            plain = false;
                            others.extend(alias.as_ref().map(|a| a.name.value.clone()).or_else(|| name.0.last().and_then(|p| p.as_ident()).map(|i| i.value.clone())));
                        }
                        (Reads::With(at), _) => {
                            plain = false;
                            let (cte_name, body, sees, columns) = &ctes[at];
                            let by = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| cte_name.clone());
                            match body {
                                Some(body) => {
                                    let mut renamed = columns.clone();
                                    for (at, name) in listed(alias).into_iter().enumerate() {
                                        match renamed.get_mut(at) {
                                            Some(slot) => *slot = name,
                                            None => renamed.push(name),
                                        }
                                    }
                                    derived.push((by, body, &ctes[..*sees], renamed));
                                }
                                None => others.push(by),
                            }
                        }
                        (Reads::Table, Some(table)) => {
                            let by = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| table.clone());
                            tables.push((by, table));
                        }
                        (Reads::Table, None) => {
                            plain = false;
                            others.extend(alias.as_ref().map(|a| a.name.value.clone()).or_else(|| name.0.last().and_then(|p| p.as_ident()).map(|i| i.value.clone())));
                        }
                    }
                }
                TableFactor::Derived { lateral, subquery, alias, .. } => {
                    plain = false;
                    compared_query(subquery, ctes, enclosed || *lateral, names, out);
                    match (lateral, alias) {
                        (false, Some(a)) => derived.push((a.name.value.clone(), subquery.as_ref(), ctes, listed(alias))),
                        (_, a) => others.extend(a.as_ref().map(|a| a.name.value.clone())),
                    }
                }
                TableFactor::JsonTable { alias, .. } | TableFactor::TableFunction { alias, .. } | TableFactor::Function { alias, .. } => {
                    plain = false;
                    others.extend(alias.as_ref().map(|a| a.name.value.clone()));
                }
                _ => plain = false,
            }
        }
    }
    let column = |e: &Expr| -> Vec<(String, String)> {
        match e {
            Expr::Identifier(i) if plain && tables.len() == 1 => vec![(tables[0].1.clone(), i.value.to_ascii_lowercase())],
            Expr::Identifier(i) if items == 1 && derived.len() == 1 => crate::mysql_keys::output_origins(derived[0].1, derived[0].2, enclosed, &derived[0].3, &i.value.to_ascii_lowercase()),
            Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
                let name = parts[1].value.to_ascii_lowercase();
                let items: Vec<&str> = tables.iter().map(|(b, _)| b.as_str()).chain(derived.iter().map(|(b, _, _, _)| b.as_str())).chain(others.iter().map(String::as_str)).collect();
                match crate::mysql_names::item(items.iter().copied(), &parts[0].value) {
                    // Spelled otherwise than the item, it may be an enclosing item spelled so.
                    Some(i) if enclosed && items[i] != parts[0].value => Vec::new(),
                    Some(i) if i < tables.len() => vec![(tables[i].1.clone(), name)],
                    Some(i) => derived.get(i - tables.len()).map(|(_, q, sees, columns)| crate::mysql_keys::output_origins(q, sees, enclosed, columns, &name)).unwrap_or_default(),
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    };
    fn walk(e: &Expr, names: &Names, column: &dyn Fn(&Expr) -> Vec<(String, String)>, out: &mut Vec<(String, String, Target)>) {
        use ast::BinaryOperator::*;
        let pair = |col: &Expr, p: &Expr, out: &mut Vec<(String, String, Target)>| {
            if let Some(param) = direct(p, names) {
                for (table, name) in column(col) {
                    out.push((param.clone(), table, Target::Name(name)));
                }
            }
        };
        match e {
            Expr::BinaryOp { left, op: Eq | NotEq | Lt | LtEq | Gt | GtEq | Spaceship, right } => {
                pair(left, right, out);
                pair(right, left, out);
            }
            Expr::BinaryOp { left, op: And | Or | Xor, right } => {
                walk(left, names, column, out);
                walk(right, names, column, out);
            }
            Expr::InList { expr, list, .. } => {
                for item in list {
                    pair(expr, item, out);
                }
            }
            Expr::Between { expr, low, high, .. } => {
                pair(expr, low, out);
                pair(expr, high, out);
            }
            Expr::Nested(inner) | Expr::UnaryOp { expr: inner, .. } => walk(inner, names, column, out),
            _ => {}
        }
    }
    let ons = from.iter().flat_map(|twj| twj.joins.iter()).filter_map(|j| match &j.join_operator {
        ast::JoinOperator::Join(ast::JoinConstraint::On(e))
        | ast::JoinOperator::Inner(ast::JoinConstraint::On(e))
        | ast::JoinOperator::Left(ast::JoinConstraint::On(e))
        | ast::JoinOperator::LeftOuter(ast::JoinConstraint::On(e))
        | ast::JoinOperator::Right(ast::JoinConstraint::On(e))
        | ast::JoinOperator::RightOuter(ast::JoinConstraint::On(e))
        | ast::JoinOperator::CrossJoin(ast::JoinConstraint::On(e))
        | ast::JoinOperator::StraightJoin(ast::JoinConstraint::On(e)) => Some(e),
        _ => None,
    });
    for filter in filters.into_iter().chain(ons) {
        walk(filter, names, &column, out);
    }
    if let Some((having, aliases)) = having {
        let unaliased = |e: &Expr| match e {
            Expr::Identifier(i) if aliases.iter().any(|a| a.eq_ignore_ascii_case(&i.value)) => Vec::new(),
            _ => column(e),
        };
        walk(having, names, &unaliased, out);
    }
}

/// The parameters the caller may pass null for, by name, and each one cast to `nullable(…)` that is
/// written whole to a column refusing null, as `(parameter, table, column)`.
pub(crate) struct Nullable {
    pub params: HashSet<String>,
    pub refused: Vec<(String, String, String)>,
}

/// The parameters of `statement` the caller may pass null for: those `marked` with `nullable(…)`,
/// and the bare ones written whole to a column that may hold null.
pub(crate) fn nullable(statement: &Statement, names: &Names, host: &Host, marked: &[String]) -> Lazy<Nullable> {
    let mut out = Nullable { params: marked.iter().cloned().collect(), refused: Vec::new() };
    let filled_all = filled(statement, names, Fill::Written);
    let bare = filled(statement, names, Fill::Bare);
    let marked_writes: Vec<&(String, String, Target)> = filled_all.iter().filter(|(param, _, _)| marked.contains(param)).collect();
    if bare.is_empty() && marked_writes.is_empty() {
        return Some(out);
    }
    // Every table a parameter is written to whole, as `prefetch` asked.
    let tables = written(statement, names);
    let (rows, checks) = (crate::mysql::answered_by_table(host, COLUMNS_QUERY, &tables), by_table(host, CHECKS_QUERY, &tables));
    let (rows, checks) = (rows?, checks?);
    let (rows, answers) = match rows {
        Ok((rows, answers)) => (Ok(rows), answers),
        Err(e) => (Err(e), Default::default()),
    };
    // Per table as the server names it, the CHECKs.
    let mut checked: HashMap<String, Vec<String>> = HashMap::new();
    for row in checks.unwrap_or_default() {
        if let (Some(table), Some(clause)) = (row.first().and_then(as_str), row.get(1).and_then(as_str)) {
            checked.entry(table.to_string()).or_default().push(clause.to_string());
        }
    }
    let rows = rows.unwrap_or_default();
    let generated = refused_by(&rows, &checked);
    // Per table as the server names it: each column in lower case, whether it may hold NULL, and
    // whether it is INVISIBLE.
    let mut columns: HashMap<String, Vec<(String, bool, bool)>> = HashMap::new();
    for row in rows {
        let (Some(table), Some(column), Some(nullable)) = (row.first().and_then(as_str), row.get(1).and_then(as_str), row.get(2).and_then(as_str)) else { continue };
        let invisible = row.get(3).and_then(as_str).is_some_and(|extra| extra.to_ascii_uppercase().contains("INVISIBLE"));
        let column = column.to_ascii_lowercase();
        let refused = checked.get(table).is_some_and(|clauses| clauses.iter().any(|clause| refuses_null(clause, &column))) || generated.get(table).is_some_and(|refused| refused.contains(&column));
        columns.entry(table.to_string()).or_default().push((column, nullable.eq_ignore_ascii_case("YES") && !refused, invisible));
    }
    // The column a target names, and whether it may hold NULL.
    let column_of = |table: &String, target: &Target| {
        // The server's name for the table ([`crate::mysql_names::Answers`]).
        let known = match columns.get(table) {
            Some(known) => known,
            None => columns.get(answers.name(table)?)?,
        };
        let (column, nullable, _) = match target {
            Target::Name(name) => known.iter().find(|(c, _, _)| c == name),
            Target::Position(i) => known.iter().filter(|(_, _, invisible)| !invisible).nth(*i),
        }?;
        Some((column.clone(), *nullable))
    };
    let mut fills: HashMap<String, bool> = HashMap::new();
    for (param, table, target) in bare {
        if let Some((_, nullable)) = column_of(&table, &target) {
            *fills.entry(param).or_insert(true) &= nullable;
        }
    }
    out.params.extend(fills.into_iter().filter(|(_, nullable)| *nullable).map(|(param, _)| param));
    for (param, table, target) in marked_writes {
        if let Some((column, false)) = column_of(table, target) {
            out.refused.push((param.clone(), table.clone(), column));
        }
    }
    Some(out)
}

/// Per table, the columns (in lower case) a NULL in which a generated column refusing NULL, by its
/// NOT NULL (error 1048, naming the generated column) or one of the table's CHECKs (`checked`),
/// refuses, from [`COLUMNS_QUERY`]'s rows: those its expression is NULL for wherever they are NULL
/// ([`probe`]), through the generated columns it reads in turn ([`refusal::refused_by_generated`]).
/// A column an expression gives a value for, as `coalesce` does, may be NULL, and so may any
/// column of an expression the parser does not read.
fn refused_by(rows: &[Vec<serde_json::Value>], checked: &HashMap<String, Vec<String>>) -> HashMap<String, HashSet<String>> {
    // Per table, each column in lower case, and each generated one with whether it refuses NULL and
    // its expression.
    let mut columns: HashMap<&str, Vec<String>> = HashMap::new();
    let mut generated: HashMap<&str, Vec<(String, bool, Probe)>> = HashMap::new();
    for row in rows {
        let (Some(table), Some(column)) = (row.first().and_then(as_str), row.get(1).and_then(as_str)) else { continue };
        let column = column.to_ascii_lowercase();
        columns.entry(table).or_default().push(column.clone());
        let (Some(nullable), Some(extra), Some(expression)) = (row.get(2).and_then(as_str), row.get(3).and_then(as_str), row.get(4).and_then(as_str)) else { continue };
        let extra = extra.to_ascii_uppercase();
        if !extra.contains("GENERATED") || extra.contains("DEFAULT_GENERATED") {
            continue;
        }
        let read = crate::mysql::check_clause(expression).and_then(|text| crate::mysql::parse_expr(&text));
        let refused = nullable.eq_ignore_ascii_case("NO") || checked.get(table).is_some_and(|clauses| clauses.iter().any(|clause| refuses_null(clause, &column)));
        generated.entry(table).or_default().push((column, refused, read.as_ref().map_or(Probe::Unknown, probe)));
    }
    generated.into_iter().map(|(table, generated)| (table.to_string(), refusal::refused_by_generated(&columns[table], &generated))).collect()
}

/// Whether a CHECK, as the server prints it (`(`email` is not null)`), refuses null in `column`
/// (lower case): it is false wherever the column is NULL ([`refusal::check_refuses`]), as IS NOT
/// NULL is, and as `coalesce` with a default that fails it, `<=>` and IS TRUE are. A CHECK that
/// is NULL there passes, as one that only compares the column is.
fn refuses_null(clause: &str, column: &str) -> bool {
    crate::mysql::parse_expr(clause).is_some_and(|e| refusal::check_refuses(&probe(&e), column))
}

/// A CHECK or a generated column's expression, as the server prints it, as [`refusal::with_null`]
/// reads it, as MySQL evaluates it: a column by its name in lower case; NULL, a boolean, an
/// integer a double holds exactly (as MySQL compares a number with a string) and any other
/// literal; IS [NOT] NULL, IS [NOT] TRUE, IS [NOT] FALSE and `<=>`; comparisons; AND, OR and NOT;
/// IF, and a CASE without an operand; COALESCE, IFNULL and NULLIF; and every operator and call
/// NULL wherever an operand is ([`crate::mysql_nulls::strict_operands`]). Two integers compare as
/// integers, in a comparison, `<=>` and NULLIF, only where one side is a number written out, which
/// makes the comparison numeric: what COALESCE, IF or CASE gives takes the type its operands share,
/// a column's among them, and MySQL compares two strings as text, `'1.00'` apart from `'1'`.
fn probe(e: &Expr) -> Probe {
    use ast::BinaryOperator as Op;
    let boxed = |e: &Expr| Box::new(probe(e));
    fn number(e: &Expr) -> bool {
        match e {
            Expr::Nested(inner) => number(inner),
            Expr::Value(v) => matches!(v.value, ast::Value::Number(..)),
            _ => false,
        }
    }
    match e {
        Expr::Nested(inner) => probe(inner),
        Expr::Value(v) => match &v.value {
            ast::Value::Null => Probe::Null,
            ast::Value::Boolean(b) => Probe::Int(*b as i64),
            ast::Value::Number(n, _) => n.parse().ok().filter(|n: &i64| n.unsigned_abs() <= 1 << 53).map_or(Probe::Value, Probe::Int),
            _ => Probe::Value,
        },
        Expr::Identifier(i) => Probe::Column(i.value.to_ascii_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map_or(Probe::Unknown, |i| Probe::Column(i.value.to_ascii_lowercase())),
        Expr::IsNull(x) => Probe::IsNull { arg: boxed(x), not: false },
        Expr::IsNotNull(x) => Probe::IsNull { arg: boxed(x), not: true },
        Expr::IsTrue(x) => Probe::IsTrue { arg: boxed(x), not: false },
        Expr::IsNotTrue(x) => Probe::IsTrue { arg: boxed(x), not: true },
        Expr::IsFalse(x) => Probe::IsFalse { arg: boxed(x), not: false },
        Expr::IsNotFalse(x) => Probe::IsFalse { arg: boxed(x), not: true },
        Expr::BinaryOp { left, op: Op::Spaceship, right } => Probe::Same { left: boxed(left), right: boxed(right), not: false, exact: number(left) || number(right) },
        Expr::BinaryOp { left, op: op @ (Op::Eq | Op::NotEq | Op::Lt | Op::Gt | Op::LtEq | Op::GtEq), right } => {
            let op = match op {
                Op::Eq => Cmp::Eq,
                Op::NotEq => Cmp::NotEq,
                Op::Lt => Cmp::Lt,
                Op::Gt => Cmp::Gt,
                Op::LtEq => Cmp::LtEq,
                _ => Cmp::GtEq,
            };
            Probe::Compare { op, left: boxed(left), right: boxed(right), exact: number(left) || number(right) }
        }
        Expr::BinaryOp { left, op: Op::And, right } => Probe::And(boxed(left), boxed(right)),
        Expr::BinaryOp { left, op: Op::Or, right } => Probe::Or(boxed(left), boxed(right)),
        Expr::UnaryOp { op: ast::UnaryOperator::Not, expr } => Probe::Not(boxed(expr)),
        Expr::Case { operand: None, conditions, else_result, .. } => Probe::Case { arms: conditions.iter().map(|w| (probe(&w.condition), probe(&w.result))).collect(), otherwise: else_result.as_deref().map(boxed) },
        Expr::Function(f) if f.over.is_none() && f.filter.is_none() && f.within_group.is_empty() => {
            let args: Option<Vec<&Expr>> = match &f.args {
                ast::FunctionArguments::List(list) if list.duplicate_treatment.is_none() && list.clauses.is_empty() => list.args.iter().map(arg_expr).collect(),
                _ => None,
            };
            match (crate::mysql::builtin(f).as_str(), args.as_deref()) {
                ("if", Some([test, then, otherwise])) => Probe::Case { arms: vec![(probe(test), probe(then))], otherwise: Some(boxed(otherwise)) },
                ("coalesce" | "ifnull", Some(args)) if !args.is_empty() => Probe::Coalesce(args.iter().map(|a| probe(a)).collect()),
                ("nullif", Some([value, other])) => Probe::NullIf { value: boxed(value), other: boxed(other), exact: number(value) || number(other) },
                _ => strict(e),
            }
        }
        _ => strict(e),
    }
}

/// An operator or a call NULL wherever one of its strict operands is, else unknown.
fn strict(e: &Expr) -> Probe {
    let operands = crate::mysql_nulls::strict_operands(e);
    if operands.is_empty() { Probe::Unknown } else { Probe::Strict(operands.into_iter().map(probe).collect()) }
}

/// The tables of the current schema the statement reads and writes, as it names them, for the
/// errors it may raise ([`crate::errors`]): every table a FROM item reads, in subqueries and WITH
/// queries too, and the tables an INSERT, an UPDATE or a DELETE writes: a multi-table UPDATE's or
/// DELETE's every table, any of which it may write. A WITH query is no table, and a table of another
/// schema is left out, since the catalog is read from the current one; a write that names one, or
/// that writes through something that is no table, lists nothing it writes.
pub(crate) fn tables(statement: &Statement) -> Tables {
    let mut walk = Walk { tables: Vec::new(), by_schema: false };
    walk.statement(statement, &Env::default());
    let target = |relation: &TableFactor| match relation {
        TableFactor::Table { name, .. } => table_name(name),
        _ => None,
    };
    let write = match statement {
        Statement::Insert(insert) => match &insert.table {
            ast::TableObject::TableName(name) => table_name(name).into_iter().collect(),
            _ => Vec::new(),
        },
        Statement::Update(update) if update.table.joins.is_empty() && update.from.is_none() => target(&update.table.relation).into_iter().collect(),
        Statement::Update(update) => {
            let from = match &update.from {
                Some(ast::UpdateTableFromKind::BeforeSet(from) | ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
                None => &[],
            };
            every_table(std::iter::once(&update.table).chain(from)).unwrap_or_default()
        }
        Statement::Delete(delete) => {
            let (ast::FromTable::WithFromKeyword(from) | ast::FromTable::WithoutKeyword(from)) = &delete.from;
            match from.as_slice() {
                [only] if delete.tables.is_empty() && only.joins.is_empty() && delete.using.is_none() => target(&only.relation).into_iter().collect(),
                _ => every_table(from.iter().chain(delete.using.iter().flatten())).unwrap_or_default(),
            }
        }
        _ => Vec::new(),
    };
    Tables { read: walk.tables, write, ..Default::default() }
}

/// Every table of a multi-table UPDATE's or DELETE's tables, or `None` where one is a table of
/// another schema or something that is no table, which the write might reach through.
fn every_table<'a>(from: impl Iterator<Item = &'a ast::TableWithJoins>) -> Option<Vec<String>> {
    fn factor(f: &TableFactor, out: &mut Vec<String>) -> Option<()> {
        match f {
            TableFactor::Table { name, .. } => out.push(table_name(name)?),
            TableFactor::NestedJoin { table_with_joins, .. } => joined(table_with_joins, out)?,
            _ => return None,
        }
        Some(())
    }
    fn joined(t: &ast::TableWithJoins, out: &mut Vec<String>) -> Option<()> {
        factor(&t.relation, out)?;
        t.joins.iter().try_for_each(|j| factor(&j.relation, out))
    }
    let mut out = Vec::new();
    for t in from {
        joined(t, &mut out)?;
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// The column a parameter is written to whole, by name or by its place in the table.
#[derive(Debug, PartialEq)]
enum Target {
    Name(String),
    Position(usize),
}

/// The parameters `statement` writes whole and bare to a column: a VALUES item, a SET's value, a
/// column of the SELECT that feeds an INSERT. By the tree alone, whatever schema the table is in.
pub(crate) fn written_whole(statement: &Statement, names: &Names) -> HashSet<String> {
    filled(statement, names, Fill::Bare).into_iter().map(|(param, _, _)| param).collect()
}

/// Every table `statement` writes a parameter to whole, bare or cast, the SELECT that feeds an
/// INSERT included: the tables whose columns [`nullable`] reads.
fn written(statement: &Statement, names: &Names) -> Vec<String> {
    let mut tables = written_tables(&filled(statement, names, Fill::Written));
    tables.sort();
    tables.dedup();
    tables
}

fn written_tables(filled: &[(String, String, Target)]) -> Vec<String> {
    let mut tables: Vec<String> = filled.iter().map(|(_, table, _)| table.clone()).collect();
    tables.sort();
    tables.dedup();
    tables
}

/// The table name a statement writes to, when it is one of the current schema.
fn table_name(name: &ast::ObjectName) -> Option<String> {
    match name.0.as_slice() {
        [ast::ObjectNamePart::Identifier(i)] => Some(i.value.clone()),
        _ => None,
    }
}

/// The parameter an expression is, whole: under casts, parentheses and the `JSON_COMPACT()` a
/// `::json` cast compiles to on MariaDB, which leave a NULL as it is.
fn direct(e: &Expr, names: &Names) -> Option<String> {
    match e {
        Expr::Value(v) => match &v.value {
            ast::Value::Placeholder(p) if p == "?" => names.get(&(v.span.start.line, v.span.start.column)).cloned(),
            _ => None,
        },
        Expr::Nested(inner) | Expr::Cast { expr: inner, .. } | Expr::Convert { expr: inner, .. } => direct(inner, names),
        _ => json_compact(e).and_then(|inner| direct(inner, names)),
    }
}

/// The argument of `JSON_COMPACT(x)`, which MariaDB writes for a `::json` cast (it has no
/// `CAST(… AS JSON)`), and which is NULL for NULL.
fn json_compact(e: &Expr) -> Option<&Expr> {
    let Expr::Function(f) = e else { return None };
    let ast::FunctionArguments::List(list) = &f.args else { return None };
    match list.args.as_slice() {
        [only] if crate::mysql::builtin(f) == "json_compact" && list.clauses.is_empty() && f.over.is_none() => arg_expr(only),
        _ => None,
    }
}

/// The parameter an expression is, bare: under parentheses alone, no cast.
fn bare(e: &Expr, names: &Names) -> Option<String> {
    match e {
        Expr::Value(v) => match &v.value {
            ast::Value::Placeholder(p) if p == "?" => names.get(&(v.span.start.line, v.span.start.column)).cloned(),
            _ => None,
        },
        Expr::Nested(inner) => bare(inner, names),
        _ => None,
    }
}

/// Which parameters written whole to a column [`filled`] lists.
#[derive(Clone, Copy, PartialEq)]
enum Fill {
    /// Bare or under the casts, the SELECT that feeds an INSERT aside: those that take the
    /// column's type.
    Typed,
    /// Only one with no cast, the columns of the one SELECT that feeds an INSERT included: those
    /// that take the column's nullability.
    Bare,
    /// Bare or under the casts, the columns of the one SELECT that feeds an INSERT included: every
    /// write a parameter cast to `nullable(…)` is checked against.
    Written,
}

/// Every parameter written whole to a column, as `fill` picks them: (parameter, table as the
/// statement names it, column).
fn filled(statement: &Statement, names: &Names, fill: Fill) -> Vec<(String, String, Target)> {
    let direct = |e: &Expr, names: &Names| if fill == Fill::Bare { bare(e, names) } else { direct(e, names) };
    let mut out = Vec::new();
    let column = |target: &ast::AssignmentTarget| match target {
        ast::AssignmentTarget::ColumnName(name) => name.0.last().and_then(|p| p.as_ident()).map(|i| (name.0.len(), i.value.to_ascii_lowercase())),
        ast::AssignmentTarget::Tuple(_) => None,
    };
    match statement {
        Statement::Insert(insert) => {
            let ast::TableObject::TableName(name) = &insert.table else { return out };
            let Some(table) = table_name(name) else { return out };
            let columns: Vec<String> = insert.columns.iter().filter_map(|c| c.0.last().and_then(|p| p.as_ident()).map(|i| i.value.to_ascii_lowercase())).collect();
            if columns.len() != insert.columns.len() {
                return out;
            }
            let target_at = |i: usize| if columns.is_empty() { Some(Target::Position(i)) } else { columns.get(i).map(|c| Target::Name(c.clone())) };
            match insert.source.as_ref().filter(|q| q.with.is_none()).map(|q| q.body.as_ref()) {
                Some(SetExpr::Values(values)) => {
                    for row in &values.rows {
                        for (i, value) in row.content.iter().enumerate() {
                            let Some(param) = direct(value, names) else { continue };
                            if let Some(target) = target_at(i) {
                                out.push((param, table.clone(), target));
                            }
                        }
                    }
                }
                // The SELECT that feeds the INSERT writes its columns whole, as a VALUES row does.
                Some(SetExpr::Select(select)) if fill != Fill::Typed => {
                    for (i, item) in select.projection.iter().enumerate() {
                        let (SelectItem::UnnamedExpr(value) | SelectItem::ExprWithAlias { expr: value, .. }) = item else { break };
                        let Some(param) = direct(value, names) else { continue };
                        if let Some(target) = target_at(i) {
                            out.push((param, table.clone(), target));
                        }
                    }
                }
                _ => {}
            }
            let update = match &insert.on {
                Some(ast::OnInsert::DuplicateKeyUpdate(set)) => set.as_slice(),
                _ => &[],
            };
            for a in insert.assignments.iter().chain(update) {
                if let (Some(param), Some((_, name))) = (direct(&a.value, names), column(&a.target)) {
                    out.push((param, table.clone(), Target::Name(name)));
                }
            }
        }
        Statement::Update(update) => {
            // The tables the statement names, by the name it uses for each.
            let mut tables: Vec<(String, String)> = Vec::new();
            let mut plain = update.from.is_none() && update.table.joins.is_empty();
            for factor in std::iter::once(&update.table.relation).chain(update.table.joins.iter().map(|j| &j.relation)) {
                match factor {
                    TableFactor::Table { name, alias, .. } => match table_name(name) {
                        Some(table) => {
                            let by = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| table.clone());
                            tables.push((by, table));
                        }
                        None => plain = false,
                    },
                    _ => plain = false,
                }
            }
            for a in &update.assignments {
                let (Some(param), Some((parts, name))) = (direct(&a.value, names), column(&a.target)) else { continue };
                let table = match (&a.target, parts) {
                    // A bare column is the one table's, where there is one.
                    (_, 1) if plain => tables.first().map(|(_, t)| t.clone()),
                    (ast::AssignmentTarget::ColumnName(n), 2) => {
                        let by = n.0.first().and_then(|p| p.as_ident());
                        by.and_then(|by| crate::mysql_names::item(tables.iter().map(|(b, _)| b.as_str()), &by.value)).map(|i| tables[i].1.clone())
                    }
                    _ => None,
                };
                if let Some(table) = table {
                    out.push((param, table, Target::Name(name)));
                }
            }
        }
        _ => {}
    }
    out
}

/// What a place in the statement sees: the WITH queries in scope, by name.
#[derive(Clone, Default)]
struct Env {
    ctes: Vec<String>,
}

/// A walk of the statement for the tables of the current schema its FROM items read.
struct Walk {
    /// Each table of the current schema a FROM item reads, as the statement names it.
    tables: Vec<String>,
    /// A FROM item names a table by its schema (`db.t`), which `tables` leaves out.
    by_schema: bool,
}

/// Whether a FROM item of the statement names a table by its schema (`db.t`, its own schema's
/// included), which [`tables`] leaves out.
pub(crate) fn reads_by_schema(statement: &Statement) -> bool {
    let mut walk = Walk { tables: Vec::new(), by_schema: false };
    walk.statement(statement, &Env::default());
    walk.by_schema
}

impl Walk {
    fn statement(&mut self, statement: &Statement, env: &Env) {
        match statement {
            Statement::Query(q) => self.query(q, env),
            Statement::Insert(insert) => {
                if let Some(source) = &insert.source {
                    self.query(source, env);
                }
                let update = match &insert.on {
                    Some(ast::OnInsert::DuplicateKeyUpdate(set)) => set.as_slice(),
                    _ => &[],
                };
                for a in insert.assignments.iter().chain(update) {
                    self.expr(&a.value, env);
                }
            }
            Statement::Update(update) => {
                let mut from = vec![update.table.clone()];
                if let Some(ast::UpdateTableFromKind::BeforeSet(more) | ast::UpdateTableFromKind::AfterSet(more)) = &update.from {
                    from.extend(more.iter().cloned());
                }
                self.level(&from, env);
                for a in &update.assignments {
                    self.expr(&a.value, env);
                }
                for e in update.selection.iter().chain(update.order_by.iter().map(|o| &o.expr)).chain(update.limit.iter()) {
                    self.expr(e, env);
                }
            }
            Statement::Delete(delete) => {
                let (ast::FromTable::WithFromKeyword(from) | ast::FromTable::WithoutKeyword(from)) = &delete.from;
                let mut from = from.clone();
                from.extend(delete.using.iter().flatten().cloned());
                self.level(&from, env);
                for e in delete.selection.iter().chain(delete.order_by.iter().map(|o| &o.expr)).chain(delete.limit.iter()) {
                    self.expr(e, env);
                }
            }
            _ => {}
        }
    }

    fn query(&mut self, q: &Query, env: &Env) {
        let mut env = env.clone();
        for cte in q.with.iter().flat_map(|w| &w.cte_tables) {
            self.query(&cte.query, &env);
            env.ctes.push(cte.alias.name.value.clone());
        }
        self.body(&q.body, &env);
        for o in q.order_by.iter().flat_map(|o| match &o.kind {
            ast::OrderByKind::Expressions(list) => list.as_slice(),
            _ => &[],
        }) {
            self.expr(&o.expr, &env);
        }
    }

    fn body(&mut self, body: &SetExpr, env: &Env) {
        match body {
            SetExpr::Select(s) => self.select(s, env),
            SetExpr::Query(q) => self.query(q, env),
            SetExpr::SetOperation { left, right, .. } => {
                self.body(left, env);
                self.body(right, env);
            }
            SetExpr::Values(values) => {
                for e in values.rows.iter().flat_map(|row| &row.content) {
                    self.expr(e, env);
                }
            }
            SetExpr::Insert(s) | SetExpr::Update(s) | SetExpr::Delete(s) | SetExpr::Merge(s) => self.statement(s, env),
            _ => {}
        }
    }

    fn select(&mut self, s: &Select, env: &Env) {
        self.level(&s.from, env);
        for item in &s.projection {
            if let SelectItem::ExprWithAlias { expr, .. } | SelectItem::UnnamedExpr(expr) = item {
                self.expr(expr, env);
            }
        }
        let group_by = match &s.group_by {
            ast::GroupByExpr::Expressions(list, _) => list.as_slice(),
            _ => &[],
        };
        for e in s.selection.iter().chain(group_by).chain(s.having.iter()).chain(s.qualify.iter()) {
            self.expr(e, env);
        }
    }

    /// The FROM items of a SELECT: the tables they read, and the subqueries and the ON conditions
    /// in them walked.
    fn level(&mut self, from: &[TableWithJoins], env: &Env) {
        let mut on: Vec<&Expr> = Vec::new();
        for twj in from {
            self.items(twj, env, &mut on);
        }
        for e in on {
            self.expr(e, env);
        }
    }

    fn items<'t>(&mut self, twj: &'t TableWithJoins, env: &Env, on: &mut Vec<&'t Expr>) {
        self.factor(&twj.relation, env, on);
        for join in &twj.joins {
            self.factor(&join.relation, env, on);
            use ast::JoinOperator as J;
            let constraint = match &join.join_operator {
                J::Join(c) | J::Inner(c) | J::Left(c) | J::LeftOuter(c) | J::Right(c) | J::RightOuter(c) | J::FullOuter(c) | J::CrossJoin(c) | J::Semi(c) | J::LeftSemi(c) | J::RightSemi(c) | J::Anti(c) | J::LeftAnti(c) | J::RightAnti(c) | J::StraightJoin(c) => Some(c),
                J::AsOf { constraint, .. } => Some(constraint),
                _ => None,
            };
            if let Some(ast::JoinConstraint::On(e)) = constraint {
                on.push(e);
            }
        }
    }

    fn factor<'t>(&mut self, factor: &'t TableFactor, env: &Env, on: &mut Vec<&'t Expr>) {
        match factor {
            TableFactor::Table { name, .. } => {
                // A WITH query in scope is what a bare name in FROM reads, whatever table shares it;
                // one a server may read as a table counts as one.
                let table = table_name(name);
                self.by_schema |= table.is_none();
                if !table.as_ref().is_some_and(|t| matches!(crate::mysql_names::with_query(env.ctes.iter().map(String::as_str), t), Reads::With(_))) {
                    self.tables.extend(table);
                }
            }
            TableFactor::Derived { subquery, .. } => self.query(subquery, env),
            TableFactor::NestedJoin { table_with_joins, .. } => self.items(table_with_joins, env, on),
            TableFactor::JsonTable { json_expr, .. } => self.expr(json_expr, env),
            TableFactor::TableFunction { expr, .. } => self.expr(expr, env),
            _ => {}
        }
    }

    /// Walks an expression, for the subqueries in it.
    fn expr(&mut self, e: &Expr, env: &Env) {
        let mut children: Vec<&Expr> = Vec::new();
        let mut queries: Vec<&Query> = Vec::new();
        match e {
            Expr::IsFalse(x) | Expr::IsNotFalse(x) | Expr::IsTrue(x) | Expr::IsNotTrue(x) | Expr::IsNull(x) | Expr::IsNotNull(x) | Expr::IsUnknown(x) | Expr::IsNotUnknown(x) | Expr::Nested(x) => children.push(x),
            Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => children.extend([a.as_ref(), b.as_ref()]),
            Expr::BinaryOp { left, right, .. } | Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => children.extend([left.as_ref(), right.as_ref()]),
            Expr::UnaryOp { expr, .. } | Expr::Cast { expr, .. } | Expr::Collate { expr, .. } | Expr::Extract { expr, .. } | Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => children.push(expr),
            Expr::Convert { expr, styles, .. } => {
                children.push(expr);
                children.extend(styles);
            }
            Expr::InList { expr, list, .. } => {
                children.push(expr);
                children.extend(list);
            }
            Expr::InSubquery { expr, subquery, .. } => {
                children.push(expr);
                queries.push(subquery);
            }
            Expr::Between { expr, low, high, .. } => children.extend([expr.as_ref(), low.as_ref(), high.as_ref()]),
            Expr::Like { expr, pattern, escape_char, .. } | Expr::ILike { expr, pattern, escape_char, .. } | Expr::SimilarTo { expr, pattern, escape_char, .. } => {
                children.extend([expr.as_ref(), pattern.as_ref()]);
                children.extend(escape_char.as_deref());
            }
            Expr::RLike { expr, pattern, .. } => children.extend([expr.as_ref(), pattern.as_ref()]),
            Expr::AtTimeZone { timestamp, time_zone } => children.extend([timestamp.as_ref(), time_zone.as_ref()]),
            Expr::Position { expr, r#in } => children.extend([expr.as_ref(), r#in.as_ref()]),
            Expr::Substring { expr, substring_from, substring_for, .. } => {
                children.push(expr);
                children.extend(substring_from.as_deref());
                children.extend(substring_for.as_deref());
            }
            Expr::Trim { expr, trim_what, trim_characters, .. } => {
                children.push(expr);
                children.extend(trim_what.as_deref());
                children.extend(trim_characters.iter().flatten());
            }
            Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
                children.extend([expr.as_ref(), overlay_what.as_ref(), overlay_from.as_ref()]);
                children.extend(overlay_for.as_deref());
            }
            Expr::Function(f) => {
                for args in [&f.parameters, &f.args] {
                    match args {
                        ast::FunctionArguments::List(list) => children.extend(list.args.iter().filter_map(arg_expr)),
                        ast::FunctionArguments::Subquery(q) => queries.push(q),
                        ast::FunctionArguments::None => {}
                    }
                }
                children.extend(f.filter.as_deref());
                children.extend(f.within_group.iter().map(|o| &o.expr));
            }
            Expr::Case { operand, conditions, else_result, .. } => {
                children.extend(operand.as_deref());
                for when in conditions {
                    children.extend([&when.condition, &when.result]);
                }
                children.extend(else_result.as_deref());
            }
            Expr::Exists { subquery, .. } => queries.push(subquery),
            Expr::Subquery(q) => queries.push(q),
            Expr::Tuple(list) => children.extend(list),
            Expr::Interval(i) => children.push(&i.value),
            Expr::JsonAccess { value, .. } => children.push(value),
            Expr::CompoundFieldAccess { root, .. } => children.push(root),
            Expr::MemberOf(m) => children.extend([m.value.as_ref(), m.array.as_ref()]),
            Expr::Prefixed { value, .. } => children.push(value),
            _ => {}
        }
        for child in children {
            self.expr(child, env);
        }
        for q in queries {
            self.query(q, env);
        }
    }
}

pub(crate) fn arg_expr(a: &FunctionArg) -> Option<&Expr> {
    match a {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(x)) | FunctionArg::Named { arg: FunctionArgExpr::Expr(x), .. } | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(x), .. } => Some(x),
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    /// What a CHECK or a generated column's expression, as MySQL 8.4 prints it, refuses.
    #[test]
    fn a_check_or_a_generated_expression_refuses_null_as_mysql_evaluates_it() {
        use reylo_engine_core::refusal::{check_refuses, with_null, Known};
        let probe = |text: &str| super::probe(&crate::mysql::parse_expr(text).expect(text));
        for (check, refuses) in [
            ("(coalesce(`a`,0) > 0)", true),
            ("(`a` <=> 1)", true),
            ("(`a` is true)", true),
            ("((`a` is null) = 0)", true),
            ("(not((`a` is null)))", true),
            ("((`b` > 0) and (`a` is not null))", true),
            ("(coalesce(`a`,1) > 0)", false),
            ("(`a` is not false)", false),
            ("((`a` is null) or (`a` > 0))", false),
            ("((`a` is not null) or (`b` is not null))", false),
            // A VARCHAR's COALESCE is a string, which MySQL orders as text: '10' is before '9'.
            ("((coalesce(`a`,10) < coalesce(`a`,9)) or (`a` is not null))", false),
            // Over a DECIMAL `a`, the left IF is the string '1.00' and the right '1'.
            ("(if((`a` is null),coalesce(`a`,1),_utf8mb4'a') <> if((`a` is null),1,_utf8mb4'b'))", false),
        ] {
            assert_eq!(check_refuses(&probe(check), "a"), refuses, "{check}");
        }
        for (expression, null) in [
            ("if((`a` is null),NULL,1)", true),
            ("(if((`a` is null),NULL,1) + 1)", true),
            ("(case when (`a` > 0) then _utf8mb4'p' when (`a` <= 0) then _utf8mb4'n' end)", true),
            ("nullif(1,(`a` is null))", true),
            ("if((`a` > 0),`a`,0)", false),
            ("(case when (`a` > 0) then _utf8mb4'p' else _utf8mb4'n' end)", false),
            ("(`a` <=> 1)", false),
            ("if((`a` is null),`b`,1)", false),
            ("nullif(if((`a` is null),coalesce(`a`,1),_utf8mb4'a'),if((`a` is null),1,_utf8mb4'b'))", false),
        ] {
            assert_eq!(with_null(&probe(expression), "a") == Known::Null, null, "{expression}");
        }
    }

    use crate::api::{Answer, Need};
    use crate::host::Host;
    use serde_json::json;
    use std::collections::HashMap;

    /// The analysis of `sql` over a schema: `users (id NOT NULL, name NOT NULL, secret NOT NULL
    /// INVISIBLE, email, bio)`, `posts (id NOT NULL, user_id NOT NULL, title NOT NULL, tag)` and
    /// `accounts (id NOT NULL, email, nick, note)` with `check (email <> '' and email is not null)`,
    /// `check (nick is not null)` and `check (note <> '')`, and `measures (id NOT NULL, nu, nv, ns,
    /// nw)` with the NOT NULL generated columns `nu + 1`, `coalesce(nv, 0)`, `concat('x', ns)` and
    /// `g2 * 2` over a nullable generated `g2 = nw - 1`; each parameter with whether it may be
    /// null, and whether the columns were asked for.
    fn nullable(sql: &str) -> (Vec<(String, bool)>, bool) {
        let (analysis, asked) = analyzed(sql);
        assert!(analysis.diagnostics.is_empty(), "{sql}: {:?}", analysis.diagnostics);
        (analysis.params.iter().map(|p| (p.name.clone(), p.nullable)).collect(), asked)
    }

    /// The analysis of `sql` over a scripted catalog, and whether it asked for the columns.
    fn analyzed(sql: &str) -> (crate::api::Analysis, bool) {
        let columns = json!({ "rows": [
            ["accounts", "id", "NO", ""], ["accounts", "email", "YES", ""], ["accounts", "nick", "YES", ""], ["accounts", "note", "YES", ""],
            ["posts", "id", "NO", "auto_increment"], ["posts", "user_id", "NO", ""], ["posts", "title", "NO", ""], ["posts", "tag", "YES", ""],
            ["users", "id", "NO", "auto_increment"], ["users", "name", "NO", ""], ["users", "secret", "NO", "INVISIBLE"], ["users", "email", "YES", ""], ["users", "bio", "YES", ""],
            // As 8.4.11 prints them, a string's quotes behind one more backslash.
            ["measures", "id", "NO", "", ""], ["measures", "nu", "YES", "", ""], ["measures", "g", "NO", "STORED GENERATED", "(`nu` + 1)"],
            ["measures", "nv", "YES", "", ""], ["measures", "h", "NO", "VIRTUAL GENERATED", "coalesce(`nv`,0)"],
            ["measures", "ns", "YES", "", ""], ["measures", "j", "NO", "VIRTUAL GENERATED", "concat(_utf8mb4\\'x\\',`ns`)"],
            ["measures", "nw", "YES", "", ""], ["measures", "g2", "YES", "VIRTUAL GENERATED", "(`nw` - 1)"], ["measures", "k", "NO", "STORED GENERATED", "(`g2` * 2)"],
            ["measures", "at", "NO", "DEFAULT_GENERATED", "now()"],
        ] });
        let checks = json!({ "rows": [
            ["accounts", "((`email` <> _utf8mb4'') and (`email` is not null))"], ["accounts", "(`nick` is not null)"], ["accounts", "(`note` <> _utf8mb4'')"],
        ] });
        let mut answers: HashMap<String, Answer> = HashMap::new();
        let mut asked = false;
        for _ in 0..8 {
            let host = Host::new(&answers);
            if let Some(analysis) = crate::mysql::analyze_text(sql, &host) {
                return (analysis, asked);
            }
            for need in host.needs() {
                let (id, answer) = match need {
                    Need::Describe { id, sql: text } => {
                        let params: Vec<_> = text.matches('?').map(|_| json!({ "type": 253, "flags": 0, "charset": 255 })).collect();
                        (id, json!({ "mysql": { "columns": [], "params": params } }))
                    }
                    Need::Query { id, sql, .. } if sql == super::COLUMNS_QUERY => {
                        asked = true;
                        (id, columns.clone())
                    }
                    Need::Query { id, sql, .. } if sql == super::CHECKS_QUERY => (id, checks.clone()),
                    Need::Query { id, .. } => (id, json!({ "rows": [] })),
                };
                answers.insert(id, serde_json::from_value(answer).unwrap());
            }
        }
        panic!("the analysis never settled")
    }

    #[test]
    fn a_table_named_in_two_cases_is_one_table_where_the_server_folds_names() {
        // The server folds names: each case asked answers with the rows of the one table `users`.
        let rows = [["users", "id", "NO", ""], ["users", "name", "NO", ""], ["users", "bio", "YES", ""]];
        let sql = "update Users u join users v on u.id = 1 and v.id = 2 set u.name = :name::nullable(text), v.bio = :bio";
        let mut answers: HashMap<String, Answer> = HashMap::new();
        for _ in 0..8 {
            let host = Host::new(&answers);
            if let Some(analysis) = crate::mysql::analyze_text(sql, &host) {
                assert!(analysis.diagnostics.iter().any(|d| d.message.contains(".name, which refuses null")), "{:?}", analysis.diagnostics);
                return;
            }
            for need in host.needs() {
                let (id, answer) = match need {
                    Need::Describe { id, sql: text } => {
                        let params: Vec<_> = text.matches('?').map(|_| json!({ "type": 253, "flags": 0, "charset": 255 })).collect();
                        (id, json!({ "mysql": { "columns": [], "params": params } }))
                    }
                    Need::Query { id, sql, params, .. } if sql == super::COLUMNS_QUERY => {
                        let asked = params.first().and_then(|p| p.as_str()).unwrap_or_default().to_string();
                        (id, json!({ "rows": rows.iter().filter(|r| r[0].eq_ignore_ascii_case(&asked)).collect::<Vec<_>>() }))
                    }
                    Need::Query { id, .. } => (id, json!({ "rows": [] })),
                };
                answers.insert(id, serde_json::from_value(answer).unwrap());
            }
        }
        panic!("the analysis never settled")
    }

    /// Each parameter's type, the prepare describing every `?` with `code` as MySQL 8.4 does for a context (8 an integer one, 16 a BIT one).
    fn param_types(sql: &str, code: u32) -> Vec<(String, String)> {
        let types = json!({ "rows": [
            ["t", "id", "int", "int", ""], ["t", "big", "bigint", "bigint unsigned", ""], ["t", "flag", "tinyint", "tinyint(1)", ""], ["t", "small", "tinyint", "tinyint", ""],
            ["t", "bit1", "bit", "bit(1)", ""], ["t", "bits", "bit", "bit(8)", ""], ["t", "hidden", "bigint", "bigint", "INVISIBLE"],
            ["u", "id", "bigint", "bigint", ""], ["u", "big", "int", "int", ""],
        ] });
        let mut answers: HashMap<String, Answer> = HashMap::new();
        for _ in 0..8 {
            let host = Host::new(&answers);
            if let Some(analysis) = crate::mysql::analyze_text(sql, &host) {
                assert!(analysis.diagnostics.is_empty(), "{sql}: {:?}", analysis.diagnostics);
                return analysis.params.iter().map(|p| (p.name.clone(), p.ty.short())).collect();
            }
            for need in host.needs() {
                let (id, answer) = match need {
                    Need::Describe { id, sql: text } => {
                        let params: Vec<_> = text.matches('?').map(|_| json!({ "type": code, "flags": 0, "charset": 63 })).collect();
                        (id, json!({ "mysql": { "columns": [], "params": params } }))
                    }
                    Need::Query { id, sql, .. } if sql == super::TYPES_QUERY => (id, types.clone()),
                    // The server's default mode, under which the engine reads `NOT` as the server does.
                    Need::Query { id, sql, .. } if sql == "select @@session.sql_mode" => (id, json!({ "rows": [["ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION"]] })),
                    Need::Query { id, .. } => (id, json!({ "rows": [] })),
                };
                answers.insert(id, serde_json::from_value(answer).unwrap());
            }
        }
        panic!("the analysis never settled")
    }

    #[test]
    fn a_parameter_written_to_or_compared_with_a_column_takes_the_values_its_rows_return() {
        let p = |name: &str, ty: &str| (name.to_string(), ty.to_string());
        // A BIGINT column's rows are BigInts, a TINYINT(1)'s and a BIT(1)'s truth values, a wider BIT's bytes.
        assert_eq!(param_types("select id from t where big = :b and flag = :f and id = :i and small = :s", 8), [p("b", "bigint"), p("f", "tinyint(1)"), p("i", "int32"), p("s", "int32")]);
        assert_eq!(param_types("select id from t where bit1 = :one and bits = :eight", 16), [p("one", "bit(boolean[1])"), p("eight", "bit(boolean[8])")]);
        // Either side of a comparison, IN, BETWEEN, under NOT and OR, and a qualified column of a join.
        assert_eq!(param_types("select t.id from t join u on u.id = t.id where :b < t.big or not (u.id in (:x, :y)) and t.big between :lo and :hi", 8), [p("b", "bigint"), p("x", "bigint"), p("y", "bigint"), p("lo", "bigint"), p("hi", "bigint")]);
        // A bare column of a join is ambiguous, and a column whose two uses disagree keeps the prepare's type.
        assert_eq!(param_types("select t.id from t join u on u.id = t.id where big = :b", 8), [p("b", "int32")]);
        assert_eq!(param_types("select t.id from t, u where t.big = :b and u.big = :b2 and t.id = :c and u.id = :c2", 8), [p("b", "bigint"), p("b2", "int32"), p("c", "int32"), p("c2", "bigint")]);
        // Written whole: VALUES by name and by place (the INVISIBLE column is no place), and an UPDATE's SET and WHERE.
        assert_eq!(param_types("insert into t (big, flag) values (:b, :f)", 8), [p("b", "bigint"), p("f", "tinyint(1)")]);
        assert_eq!(param_types("insert into t values (:i, :b, :f, :s, :one, :eight)", 8), [p("i", "int32"), p("b", "bigint"), p("f", "tinyint(1)"), p("s", "int32"), p("one", "bit(boolean[1])"), p("eight", "bit(boolean[8])")]);
        assert_eq!(param_types("update t set flag = :f where big = :b", 8), [p("f", "tinyint(1)"), p("b", "bigint")]);
        assert_eq!(param_types("delete from t where big = :b", 8), [p("b", "bigint")]);
        // An expression of the column is not the column: `big + 1` is the prepare's.
        assert_eq!(param_types("select id from t where big + 1 = :b limit :n", 8), [p("b", "int32"), p("n", "int32")]);
        // A HAVING reads a bare name as the select list's alias first, and an alias of anything but
        // the column itself is no column.
        assert_eq!(param_types("select count(*) as big from t having big = :b", 8), [p("b", "int32")]);
        assert_eq!(param_types("select id as Flag from t having flag = :f", 8), [p("f", "int32")]);
        assert_eq!(param_types("select big from t having big = :b", 8), [p("b", "bigint")]);
        assert_eq!(param_types("select t.big as big from t having big = :b", 8), [p("b", "bigint")]);
        assert_eq!(param_types("select count(*) as big from t where big = :w having big > :h", 8), [p("w", "bigint"), p("h", "int32")]);
    }

    /// The tables `sql` reads and writes, as [`super::tables`] names them.
    fn tables(sql: &str) -> (Vec<String>, Vec<String>) {
        let statement = crate::mysql::parse_default(sql).unwrap().pop().unwrap();
        let mut t = super::tables(&statement);
        t.read.sort();
        t.read.dedup();
        (t.read, t.write)
    }

    #[test]
    fn the_facts_are_read_for_every_table_the_statement_names_and_no_with_query() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(tables("select u.id, (select count(*) from posts p where p.user_id = u.id) from `users` u where exists (select 1 from tags t where t.id = u.id)"), (names(&["posts", "tags", "users"]), vec![]));
        // A WITH query is no table, even one named like a table; a derived table's are read.
        assert_eq!(tables("with users as (select id from accounts) select id from users join (select user_id from posts) p on p.user_id = users.id"), (names(&["accounts", "posts"]), vec![]));
        // A WITH query named in another case is the table of that name where the server keeps case,
        // and counts as read.
        assert_eq!(tables("with users as (select id from accounts) select id from Users"), (names(&["Users", "accounts"]), vec![]));
        // What an INSERT, an UPDATE and a DELETE write, and the tables they read.
        assert_eq!(tables("insert into audit (user_id) select id from users"), (names(&["users"]), names(&["audit"])));
        assert_eq!(tables("update users set name = (select max(title) from posts) where id = 1"), (names(&["posts", "users"]), names(&["users"])));
        assert_eq!(tables("delete from users where id in (select user_id from bans)"), (names(&["bans", "users"]), names(&["users"])));
        // A table of another schema is left out: the facts are read from the current one.
        assert_eq!(tables("select id from other.users"), (vec![], vec![]));
        // A multi-table UPDATE or DELETE may write any of its tables; one that names a table of
        // another schema, or joins a derived table, lists nothing it writes.
        assert_eq!(tables("delete u from users u join bans b on b.user_id = u.id").1, names(&["bans", "users"]));
        assert_eq!(tables("delete from users using users join bans on bans.user_id = users.id").1, names(&["bans", "users"]));
        assert_eq!(tables("update users u join posts p on p.user_id = u.id set p.title = u.name").1, names(&["posts", "users"]));
        assert_eq!(tables("update users u join (select 1 as id) d on d.id = u.id set u.name = 'x'").1, Vec::<String>::new());
        assert_eq!(tables("delete u from users u join other.bans b on b.user_id = u.id").1, Vec::<String>::new());
        assert_eq!(tables("insert into other.audit (user_id) values (1)").1, Vec::<String>::new());
    }

    fn may_be_null(sql: &str) -> Vec<String> {
        nullable(sql).0.into_iter().filter(|(_, n)| *n).map(|(name, _)| name).collect()
    }

    #[test]
    fn a_cast_parameter_is_its_casts_type_and_takes_no_null() {
        // What a `:name::type` cast compiles to: `CAST(? AS SIGNED)`, `CAST(? AS DECIMAL(…))`,
        // `CAST(? AS JSON)`, and on MariaDB `JSON_COMPACT(?)` for `::json`: written whole to a
        // nullable column, the parameter is a value: a cast parameter is its cast's type.
        for sql in [
            "insert into users (name, email, bio) values (:name, CAST(:email AS JSON), JSON_COMPACT(:bio))",
            "insert into users (name, email, bio) values (CAST(:name AS SIGNED), CAST(:email AS DECIMAL(10,2)), CAST(:bio AS UNSIGNED)), (JSON_COMPACT(:n2), cast(:e2 as char), CAST(:b2 AS DATETIME(6)))",
            "update users set email = CAST(:email AS SIGNED), name = JSON_COMPACT(:name), bio = JSON_COMPACT(:bio) where id = CAST(:id AS SIGNED)",
            "insert into users (name, email) values (:name, cast(:email as char)) on duplicate key update bio = CAST(:bio AS JSON), name = CAST(:again AS SIGNED)",
            "insert into users (name, email) select :name, cast(:email as char)",
            "update users set email = json_quote(:e), bio = concat(:b, '') where id = :id",
        ] {
            assert!(may_be_null(sql).is_empty(), "{sql}: {:?}", may_be_null(sql));
        }
    }

    #[test]
    fn a_column_a_not_null_generated_column_is_null_for_takes_no_null() {
        // `g`, `j` and `k` (through `g2`) are NULL wherever `nu`, `ns` and `nw` are; `h` never is.
        assert_eq!(may_be_null("insert into measures (id, nu, nv, ns, nw) values (:id, :u, :v, :s, :w)"), ["v"]);
        assert_eq!(may_be_null("update measures set nu = :u, nv = :v, nw = :w where id = :id"), ["v"]);
    }

    #[test]
    fn a_null_test_or_a_null_safe_comparison_lets_no_null_through() {
        for sql in [
            "select id from users where isnull(:k) and :a <=> email and :b is not null and :c is null",
            "select id from users where cast(:p as char) is null",
            "delete from users where :email is null",
            "with input as (select :q as q) select id from users, input where input.q is null or users.name = input.q",
            "update users set email = coalesce(JSON_COMPACT(:e), email) where JSON_COMPACT(:j) is null",
        ] {
            assert!(may_be_null(sql).iter().all(|p| p == "e"), "{sql}: {:?}", may_be_null(sql));
        }
    }

    #[test]
    fn a_parameter_cast_to_nullable_may_be_null_and_a_coalesced_one_may_not() {
        assert_eq!(may_be_null("select id from users where id = coalesce(:id::nullable(int), id) and name = coalesce(:n, name) and ifnull(:m, 1) = 1"), ["id"]);
        // Through a WITH query, which selects the parameter once and tests it for null.
        assert_eq!(may_be_null("with input as (select :q::nullable(text) as q) select id from users, input where input.q is null or users.name = input.q"), ["q"]);
        assert!(may_be_null("with input as (select :q::text as q) select id from users, input where input.q is null or users.name = input.q").is_empty());
        // Written whole to a column that holds NULL, and as typed JSON.
        assert_eq!(may_be_null("update users set email = :e::nullable(text), bio = :b::nullable(json('{\"type\": \"object\"}')) where id = :id"), ["e", "b"]);
    }

}
