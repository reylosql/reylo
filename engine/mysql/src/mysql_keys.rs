//! Whether a SELECT returns one row at most, from the unique keys of what it reads: tables, and
//! derived tables, WITH queries and views through the keys their own statements keep.
//!
//! One SELECT's FROM is flattened out of its joins into items and the equalities that may pin
//! one. Call an item **determined** when every row of the FROM carries the same row of it, or that
//! row's null extension; when every item is, the FROM has one row at most. The closure that finds
//! them, as the Postgres engine's:
//!
//! 1. A WHERE conjunct `r.k = v` for each column of a unique key of item `r`, where `v` reads no
//!    column of `r` and only columns of determined items (or no column), determines `r` in any
//!    join: the `=` rejects a null-extended row.
//! 2. The same equalities in the ON clause of an inner join determine either side, and of a LEFT
//!    JOIN its right side (RIGHT JOIN mirrors it). A FULL JOIN is refused.
//! 3. A derived table, a WITH query or a view that is one row at most is determined.
//! 4. A LATERAL derived table bounded so is determined once every other item is: it is computed
//!    anew for each row of the rest.
//!
//! A derived table's, a WITH query's or a view's unique keys are read from its statement
//! ([`output_keys`]), and pin it as a table's pin the table.
//!
//! An equality pins a key column only where it compares the column as it is: with a value of the
//! column's class ([`compares_as_is`]), and with a column only where both are exact numbers, since
//! two strings of different collations compare under one the server picks, and an exact number
//! compared with a FLOAT or DOUBLE compares as a double.

use crate::mysql::{builtin, compares_as_is, const_limit, deterministic, is_dual, is_variable, one_before_limit, reads_column, select_of, value_class, Class, Keys};
use crate::mysql_names::Reads;
use syntax::mysql::ast::{self, Distinct, Expr, GroupByExpr, JoinConstraint, JoinOperator, ObjectNamePart, Query, Select, SelectItem, SelectItemQualifiedWildcardKind, SetExpr, TableFactor, TableWithJoins};
use std::cell::RefCell;
use std::collections::HashMap;

/// One WITH query in scope: its name as written, its body (`None` for one of a RECURSIVE WITH),
/// how many of the entries before it its body sees, and the names its column list gives its
/// columns, by position.
pub(crate) type Cte<'a> = (String, Option<&'a Query>, usize, Vec<String>);

/// The WITH queries in scope, outermost first.
pub(crate) type Ctes<'a> = Vec<Cte<'a>>;

/// `outer` with a query's own WITH queries after it.
pub(crate) fn ctes_of<'a>(q: &'a Query, outer: &[Cte<'a>]) -> Ctes<'a> {
    let mut out: Ctes<'a> = outer.to_vec();
    if let Some(w) = &q.with {
        for c in &w.cte_tables {
            let body = (!w.recursive).then_some(c.query.as_ref());
            // A WITH query's body sees the ones before it, and with RECURSIVE itself too.
            let sees = out.len();
            out.push((c.alias.name.value.clone(), body, sees, c.alias.columns.iter().map(|c| c.name.value.to_ascii_lowercase()).collect()));
        }
    }
    out
}

/// One column of a key of a derived table's rows: its name in their output, in lower case, and
/// what it compares as.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    name: String,
    class: Option<Class>,
}

type Key = Vec<Part>;

enum Source<'a> {
    /// A table, by the name the statement gives it.
    Table(String),
    /// A derived table, a WITH query or a view, with the WITH queries its statement sees, and
    /// whether that statement sees the FROM items of an enclosing query ([`From::enclosed`]).
    Query(&'a Query, Ctes<'a>, bool),
    /// A LATERAL derived table, which sees the FROM items of the query it is in.
    Lateral(&'a Query, Ctes<'a>),
    /// Anything the proof does not read: a table function, a recursive WITH query.
    Other,
}

struct Item<'a> {
    /// The name its columns go by, as written.
    alias: String,
    source: Source<'a>,
    /// Whether an outer join can null-extend it.
    nullable: bool,
    /// Whether its alias renames its columns where the proof cannot follow: a table's or a LATERAL
    /// derived table's.
    renamed: bool,
    /// The names a derived table's, a WITH query's or a view's columns go by, by position, where a
    /// column list renames them.
    columns: Vec<String>,
}

struct Conjunct<'a> {
    expr: &'a Expr,
    /// For an ON clause, the items it may pin; `None` for the WHERE, which may pin any.
    may_pin: Option<Vec<usize>>,
    /// Whether it holds of every row the FROM returns.
    inner: bool,
}

/// Where a column reference points.
enum Placed {
    Item(usize, String),
    /// A column of an enclosing query.
    Outside,
    Unknown,
}

struct From<'a> {
    items: Vec<Item<'a>>,
    conjuncts: Vec<Conjunct<'a>>,
    /// Whether a USING or NATURAL join merges columns under one bare name.
    merged: bool,
    refused: Option<&'static str>,
    /// Whether the SELECT sees the FROM items of an enclosing query, as a LATERAL derived table's
    /// does and every query inside one: there a qualifier that matches one of its own items only in
    /// another case is, on a server that keeps case, an enclosing item's spelled the same.
    enclosed: bool,
    keys: &'a Keys,
    /// The WITH queries the SELECT sees, which its subqueries see too.
    ctes: Ctes<'a>,
}

impl<'a> From<'a> {
    fn read(s: &'a Select, ctes: &[Cte<'a>], enclosed: bool, keys: &'a Keys) -> From<'a> {
        let mut from = From { items: Vec::new(), conjuncts: Vec::new(), merged: false, refused: None, enclosed, keys, ctes: ctes.to_vec() };
        for twj in &s.from {
            from.flatten(twj, ctes, false);
        }
        if let Some(w) = &s.selection {
            for expr in conjuncts(w) {
                from.conjuncts.push(Conjunct { expr, may_pin: None, inner: true });
            }
        }
        from
    }

    fn flatten(&mut self, twj: &'a TableWithJoins, ctes: &[Cte<'a>], nullable: bool) {
        let start = self.items.len();
        self.factor(&twj.relation, ctes, nullable);
        for join in &twj.joins {
            enum Kind {
                Inner,
                Left,
                Right,
            }
            let (kind, constraint) = match &join.join_operator {
                JoinOperator::Join(c) | JoinOperator::Inner(c) | JoinOperator::CrossJoin(c) | JoinOperator::StraightJoin(c) => (Kind::Inner, c),
                JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => (Kind::Left, c),
                JoinOperator::Right(c) | JoinOperator::RightOuter(c) => (Kind::Right, c),
                _ => {
                    self.refused = Some("a FULL JOIN returns a row for each side that matches nothing, and the engine reads no other join than inner, left and right");
                    return;
                }
            };
            let middle = self.items.len();
            self.factor(&join.relation, ctes, nullable || matches!(kind, Kind::Left));
            let end = self.items.len();
            if matches!(kind, Kind::Right) {
                for item in &mut self.items[start..middle] {
                    item.nullable = true;
                }
            }
            match constraint {
                JoinConstraint::On(on) => {
                    let (may_pin, inner): (Vec<usize>, bool) = match kind {
                        Kind::Inner => ((start..end).collect(), true),
                        Kind::Left => ((middle..end).collect(), false),
                        Kind::Right => ((start..middle).collect(), false),
                    };
                    for expr in conjuncts(on) {
                        self.conjuncts.push(Conjunct { expr, may_pin: Some(may_pin.clone()), inner });
                    }
                }
                // They equate columns the tree does not spell out, so they pin nothing here.
                JoinConstraint::Using(_) | JoinConstraint::Natural => self.merged = true,
                JoinConstraint::None => {}
            }
        }
    }

    fn factor(&mut self, factor: &'a TableFactor, ctes: &[Cte<'a>], nullable: bool) {
        let named = |a: &ast::TableAlias| (a.name.value.clone(), !a.columns.is_empty());
        match factor {
            TableFactor::Table { name, alias, args: None, .. } => {
                let table = match name.0.as_slice() {
                    [ObjectNamePart::Identifier(t)] => Some(t.value.clone()),
                    // A view's statement names each table with the view's own schema, spelled as it
                    // is: one that differs from it in case alone is another where the server keeps case.
                    [ObjectNamePart::Identifier(s), ObjectNamePart::Identifier(t)] if self.keys.schema.as_deref() == Some(s.value.as_str()) => Some(t.value.clone()),
                    _ => None,
                };
                let Some(table) = table else {
                    let alias = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_default();
                    self.items.push(Item { alias, source: Source::Other, nullable, renamed: true, columns: Vec::new() });
                    return;
                };
                let lower = table.to_ascii_lowercase();
                let own: Vec<String> = alias.as_ref().map(|a| a.columns.iter().map(|c| c.name.value.to_ascii_lowercase()).collect()).unwrap_or_default();
                let (alias, renamed) = alias.as_ref().map(named).unwrap_or_else(|| (table.clone(), false));
                let unqualified = name.0.len() == 1;
                let reads = match unqualified {
                    true => crate::mysql_names::with_query(ctes.iter().map(|(n, _, _, _)| n.as_str()), &table),
                    false => Reads::Table,
                };
                let (source, columns) = match reads {
                    // A WITH query MySQL may read otherwise where it is read has no rows to follow,
                    // nor has a name one server reads as a WITH query and another as a table.
                    Reads::With(_) if self.keys.rebound.has(&lower) => (Source::Other, Vec::new()),
                    Reads::Unknown => (Source::Other, Vec::new()),
                    Reads::With(at) => match ctes[at].1 {
                        Some(body) => (Source::Query(body, ctes[..ctes[at].2].to_vec(), self.enclosed), overlay(&ctes[at].3, &own)),
                        None => (Source::Other, Vec::new()),
                    },
                    // A view's statement sees none of the WITH queries of the statement that reads it.
                    Reads::Table => match self.keys.view(&table) {
                        // The stored definition leaves out a view's column list, which renames its
                        // columns by position: the view's own columns, in order, are their names.
                        Some(view) => {
                            let named: Option<Vec<String>> = self.keys.columns(&table).map(|c| c.iter().map(|(name, _)| name.clone()).collect());
                            match (named, output_names(view, &[], false, self.keys)) {
                                (Some(named), Some(defined)) if named.len() == defined.len() => (Source::Query(view, Vec::new(), false), overlay(if named == defined { &[] } else { &named }, &own)),
                                _ => (Source::Other, Vec::new()),
                            }
                        }
                        None => (Source::Table(table), Vec::new()),
                    },
                };
                let renamed = renamed && matches!(source, Source::Table(_) | Source::Other);
                self.items.push(Item { alias, source, nullable, renamed, columns });
            }
            TableFactor::Derived { lateral, subquery, alias, .. } => {
                let columns: Vec<String> = alias.as_ref().map(|a| a.columns.iter().map(|c| c.name.value.to_ascii_lowercase()).collect()).unwrap_or_default();
                let (alias, renamed) = alias.as_ref().map(named).unwrap_or_default();
                let item = if *lateral { Item { alias, source: Source::Lateral(subquery, ctes.to_vec()), nullable, renamed, columns: Vec::new() } } else { Item { alias, source: Source::Query(subquery, ctes.to_vec(), self.enclosed), nullable, renamed: false, columns } };
                self.items.push(item);
            }
            TableFactor::NestedJoin { table_with_joins, alias: None } => self.flatten(table_with_joins, ctes, nullable),
            TableFactor::NestedJoin { .. } => self.refused = Some("a join with an alias of its own hides the names of the tables it joins"),
            _ => {
                let alias = match factor {
                    TableFactor::Table { alias, .. } | TableFactor::JsonTable { alias, .. } | TableFactor::Function { alias, .. } | TableFactor::UNNEST { alias, .. } => alias.as_ref(),
                    _ => None,
                };
                match alias {
                    Some(a) => self.items.push(Item { alias: a.name.value.clone(), source: Source::Other, nullable, renamed: true, columns: Vec::new() }),
                    None => self.refused = Some("a FROM item the engine cannot name"),
                }
            }
        }
    }

    /// The columns of item `i`, in lower case, where they are known.
    fn columns(&self, i: usize) -> Option<Vec<String>> {
        let item = &self.items[i];
        if item.renamed {
            return None;
        }
        match &item.source {
            Source::Table(t) => self.keys.columns(t).map(|c| c.iter().map(|(name, _)| name.clone()).collect()),
            Source::Query(q, ctes, enclosed) => renamed_names(output_names(q, ctes, *enclosed, self.keys)?, &item.columns),
            Source::Lateral(q, ctes) => output_names(q, ctes, true, self.keys),
            Source::Other => None,
        }
    }

    /// The item a qualifier names ([`crate::mysql_names::item`]).
    fn named(&self, qualifier: &str) -> Option<usize> {
        crate::mysql_names::item(self.items.iter().map(|it| it.alias.as_str()), qualifier)
    }

    /// The item a column reference reads.
    fn place(&self, parts: &[ast::Ident]) -> Placed {
        let qualified = |q: &ast::Ident, c: &ast::Ident| match self.named(&q.value) {
            // Spelled otherwise than the item, it may be an enclosing item's ([`From::enclosed`]).
            Some(i) if self.enclosed && self.items[i].alias != q.value => Placed::Unknown,
            Some(i) if !self.items[i].renamed => Placed::Item(i, c.value.to_ascii_lowercase()),
            Some(_) => Placed::Unknown,
            // Two items it may match: either may be the one it reads.
            None if self.items.iter().any(|it| crate::mysql_names::may_match(&it.alias, &q.value)) => Placed::Unknown,
            None => Placed::Outside,
        };
        match parts {
            [q, c] => qualified(q, c),
            // The view's own schema as it is spelled ([`From::factor`]).
            [s, q, c] if self.keys.schema.as_deref() == Some(s.value.as_str()) => qualified(q, c),
            [c] if !self.merged => {
                let c = &c.value.to_ascii_lowercase();
                let mut known = true;
                let mut having = Vec::new();
                for i in 0..self.items.len() {
                    match self.columns(i) {
                        Some(columns) if columns.contains(c) => having.push(i),
                        Some(_) => {}
                        None => known = false,
                    }
                }
                match having.as_slice() {
                    [i] => Placed::Item(*i, c.clone()),
                    [] if known => Placed::Outside,
                    // Without the columns, a bare name is the one item's, or the one item's
                    // whose key holds a column of the name.
                    [] if self.items.len() == 1 => Placed::Item(0, c.clone()),
                    [] => {
                        let owners: Vec<usize> = (0..self.items.len())
                            .filter(|i| matches!(&self.items[*i].source, Source::Table(t) if self.keys.keys(t).is_some_and(|k| k.iter().flatten().any(|k| k == c))))
                            .collect();
                        match owners.as_slice() {
                            [i] => Placed::Item(*i, c.clone()),
                            _ => Placed::Unknown,
                        }
                    }
                    _ => Placed::Unknown,
                }
            }
            _ => Placed::Unknown,
        }
    }

    fn place_expr(&self, e: &Expr) -> Option<Placed> {
        match e {
            Expr::Identifier(i) if !is_variable(e) => Some(self.place(std::slice::from_ref(i))),
            Expr::CompoundIdentifier(parts) => Some(self.place(parts)),
            Expr::Nested(inner) => self.place_expr(inner),
            _ => None,
        }
    }

    /// The items an expression reads, or `None` when one of its columns cannot be placed or it
    /// holds what the walk does not read.
    fn reads(&self, e: &Expr) -> Option<Vec<usize>> {
        if is_variable(e) {
            return None;
        }
        match e {
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => match self.place_expr(e)? {
                Placed::Item(i, _) => Some(vec![i]),
                Placed::Outside => Some(Vec::new()),
                Placed::Unknown => None,
            },
            Expr::Value(_) | Expr::TypedString { .. } => Some(Vec::new()),
            Expr::Nested(inner) | Expr::UnaryOp { expr: inner, .. } | Expr::Cast { expr: inner, .. } => self.reads(inner),
            Expr::BinaryOp { left, right, .. } => {
                let mut out = self.reads(left)?;
                out.extend(self.reads(right)?);
                Some(out)
            }
            Expr::Function(f) => match &f.args {
                ast::FunctionArguments::None => Some(Vec::new()),
                ast::FunctionArguments::List(list) => {
                    let mut out = Vec::new();
                    for a in &list.args {
                        match a {
                            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(x)) => out.extend(self.reads(x)?),
                            _ => return None,
                        }
                    }
                    Some(out)
                }
                ast::FunctionArguments::Subquery(_) => None,
            },
            _ => None,
        }
    }

    /// What column `column` of item `i` compares as.
    fn class(&self, i: usize, column: &str) -> Option<Class> {
        match &self.items[i].source {
            Source::Table(t) => self.keys.class_of(t, column),
            Source::Query(q, ctes, enclosed) => output_class(q, ctes, *enclosed, self.keys, &inner_name(column, &self.items[i].columns, || output_names(q, ctes, *enclosed, self.keys))?),
            Source::Lateral(q, ctes) => output_class(q, ctes, true, self.keys, column),
            Source::Other => None,
        }
    }

    /// Whether `e` is `column = value` for this column of item `i`, where the value reads no column
    /// of `i`, only columns of `done` items and of enclosing queries, gives every row one value,
    /// and compares the column as it is.
    fn equates(&self, e: &Expr, i: usize, column: &str, class: Option<Class>, done: &[bool]) -> bool {
        let own = |side: &Expr| matches!(self.place_expr(side), Some(Placed::Item(j, c)) if j == i && c == column);
        if let Expr::InSubquery { expr, subquery, negated: false } = e {
            return own(expr) && self.subquery_value(subquery, class, i, done, true);
        }
        let Expr::BinaryOp { left, op: ast::BinaryOperator::Eq, right } = e else { return false };
        [(left, right), (right, left)].into_iter().any(|(side, value)| {
            own(side)
                && match value.as_ref() {
                    Expr::Subquery(q) => self.subquery_value(q, class, i, done, false),
                    value => self.reads(value).is_some_and(|read| read.iter().all(|j| *j != i && done[*j])) && self.compares(class, value),
                }
        })
    }

    /// Whether subquery `q`, compared with a key column of item `i` that compares as `class`, is one
    /// value for every row: a plain SELECT of one column over tables, which reads no column of `i`,
    /// of this FROM only `done` items' and otherwise only its own tables' and enclosing queries',
    /// calls nothing that changes from row to row, and returns an exact number, as the key is one:
    /// as with a column, both sides must be exact numbers ([`From::compares`]). The server refuses a
    /// scalar subquery of more than one row; IN takes any number, so for it (`bounded`) the
    /// subquery must be proved to return one row at most.
    fn subquery_value(&self, q: &Query, class: Option<Class>, i: usize, done: &[bool], bounded: bool) -> bool {
        let Some(s) = plain(q) else { return false };
        let [SelectItem::UnnamedExpr(value) | SelectItem::ExprWithAlias { expr: value, .. }] = s.projection.as_slice() else { return false };
        let GroupByExpr::Expressions(group, modifiers) = &s.group_by else { return false };
        if q.with.is_some() || !modifiers.is_empty() || !class.is_some_and(Class::exact) {
            return false;
        }
        let sub = From::read(s, &self.ctes, true, self.keys);
        if sub.refused.is_some() || sub.merged || !sub.items.iter().all(|it| matches!(it.source, Source::Table(_)) && !it.renamed) {
            return false;
        }
        // Its ORDER BY picks which row a LIMIT keeps, so a column of the query around there is a
        // value per row as well.
        let ordered: Vec<&Expr> = match q.order_by.as_ref().map(|o| &o.kind) {
            None => Vec::new(),
            Some(ast::OrderByKind::Expressions(terms)) => terms.iter().map(|t| &t.expr).collect(),
            Some(_) => return false,
        };
        let limits: Vec<&Expr> = match &q.limit_clause {
            None => Vec::new(),
            Some(ast::LimitClause::LimitOffset { limit, offset, limit_by }) => limit.iter().chain(offset.iter().map(|o| &o.value)).chain(limit_by).collect(),
            Some(ast::LimitClause::OffsetCommaLimit { offset, limit }) => vec![offset, limit],
        };
        // An ON clause sees only the items its join joins, so a bare name there that is not one of
        // theirs is an enclosing query's even where a sibling of the join holds it: only a
        // qualified name is placed there.
        let on = sub.conjuncts.iter().filter(|c| c.may_pin.is_some()).map(|c| c.expr);
        let exprs = std::iter::once(value).chain(sub.conjuncts.iter().filter(|c| c.may_pin.is_none()).map(|c| c.expr)).chain(group).chain(s.having.iter()).chain(ordered).chain(limits);
        exprs.into_iter().all(|e| self.one_value_beside(&sub, e, i, done, true)) && on.into_iter().all(|e| self.one_value_beside(&sub, e, i, done, false))
            && sub.output_class(value).is_some_and(Class::exact)
            && (!bounded || at_most_one_in(q, &self.ctes, true, self.keys).is_ok())
    }

    /// Whether `e`, an expression of subquery `sub` of this FROM, reads only `sub`'s own columns,
    /// enclosing queries' and `done` items' other than `i`, names none by a bare name unless
    /// `bare`, and calls nothing that changes from row to row but `min`, `max` and `count`, which
    /// give the subquery's rows one value.
    fn one_value_beside(&self, sub: &From, e: &Expr, i: usize, done: &[bool], bare: bool) -> bool {
        let all = |e: &Expr| self.one_value_beside(sub, e, i, done, bare);
        if is_variable(e) {
            return false;
        }
        match e {
            Expr::Identifier(_) if !bare => false,
            // In an ON clause, a qualifier this FROM also has may name its item rather than the
            // subquery's: a sibling of the join, which the clause does not see, may reuse it.
            Expr::CompoundIdentifier(parts) if !bare && parts.len() >= 2 && self.items.iter().any(|it| crate::mysql_names::may_match(&it.alias, &parts[parts.len() - 2].value)) => false,
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => match sub.place_expr(e) {
                Some(Placed::Item(..)) => true,
                Some(Placed::Outside) => match self.place_expr(e) {
                    Some(Placed::Item(j, _)) => j != i && done[j],
                    Some(Placed::Outside) => true,
                    _ => false,
                },
                _ => false,
            },
            Expr::Value(_) | Expr::TypedString { .. } => true,
            Expr::Nested(inner) | Expr::UnaryOp { expr: inner, .. } | Expr::Cast { expr: inner, .. } | Expr::IsNull(inner) | Expr::IsNotNull(inner) => all(inner),
            Expr::BinaryOp { left, right, .. } => all(left) && all(right),
            Expr::InList { expr, list, .. } => all(expr) && list.iter().all(all),
            Expr::Between { expr, low, high, .. } => all(expr) && all(low) && all(high),
            Expr::Function(f) if f.over.is_none() && f.filter.is_none() && f.within_group.is_empty() => {
                let counted = matches!(builtin(f).as_str(), "min" | "max" | "count");
                (counted || deterministic(&Expr::Function(ast::Function { args: ast::FunctionArguments::None, ..f.clone() })))
                    && match &f.args {
                        ast::FunctionArguments::None => true,
                        ast::FunctionArguments::List(list) => list.clauses.is_empty() && list.args.iter().all(|a| match a {
                            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(x)) => all(x),
                            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard) => counted,
                            _ => false,
                        }),
                        ast::FunctionArguments::Subquery(_) => false,
                    }
            }
            _ => false,
        }
    }

    /// What a subquery's one column compares as: a column of its own, as `min` and `max` keep it,
    /// or a count, a BIGINT.
    fn output_class(&self, e: &Expr) -> Option<Class> {
        match e {
            Expr::Nested(inner) => self.output_class(inner),
            Expr::Function(f) if builtin(f) == "count" => Some(Class::Wide),
            Expr::Function(f) if matches!(builtin(f).as_str(), "min" | "max") => match &f.args {
                ast::FunctionArguments::List(list) if list.clauses.is_empty() => match list.args.as_slice() {
                    [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(x))] => self.output_class(x),
                    _ => None,
                },
                _ => None,
            },
            _ => match self.place_expr(e)? {
                Placed::Item(j, c) => self.class(j, &c),
                _ => None,
            },
        }
    }

    /// Whether a key column of `class` compared with `value` compares as it is, and the value is
    /// one value for every row.
    fn compares(&self, class: Option<Class>, value: &Expr) -> bool {
        if !reads_column(value) {
            return deterministic(value) && compares_as_is(class, value_class(value));
        }
        // Another column: both must be exact numbers.
        match self.place_expr(value) {
            Some(Placed::Item(j, c)) => class.is_some_and(Class::exact) && self.class(j, &c).is_some_and(Class::exact),
            _ => false,
        }
    }

    /// Whether the equalities pin every column of one of `keys`, the unique keys of item `i`.
    fn key_pinned(&self, i: usize, keys: &[Key], done: &[bool], constant: bool) -> bool {
        let item = &self.items[i];
        if item.renamed {
            return false;
        }
        let usable = |c: &Conjunct| match &c.may_pin {
            None => true,
            Some(may) => may.contains(&i) && (!constant || (c.inner && !item.nullable)),
        };
        keys.iter().any(|key| !key.is_empty() && key.iter().all(|part| self.conjuncts.iter().any(|c| usable(c) && self.equates(c.expr, i, &part.name, part.class, done))))
    }

    /// The unique keys of item `i`'s rows, each column by the name the item gives it.
    fn item_keys(&self, i: usize) -> Vec<Key> {
        if self.items[i].renamed {
            return Vec::new();
        }
        match &self.items[i].source {
            Source::Table(t) => self
                .keys
                .keys(t)
                .into_iter()
                .flatten()
                .map(|key| key.iter().map(|c| Part { name: c.clone(), class: self.keys.class_of(t, c) }).collect())
                .collect(),
            Source::Query(q, ctes, enclosed) => renamed_keys(output_keys(q, ctes, *enclosed, self.keys), &self.items[i].columns, || output_names(q, ctes, *enclosed, self.keys)),
            Source::Lateral(q, ctes) => output_keys(q, ctes, true, self.keys),
            Source::Other => Vec::new(),
        }
    }

    /// Which items are determined: the closure in the module's docs. With `constant`, only items
    /// no row null-extends count, pinned by the WHERE or by inner joins: every row of the FROM then
    /// carries the same row of each, which is what grouping and DISTINCT ask. `done` names the
    /// items taken as determined from the start, and `keys` are each item's ([`From::item_keys`]).
    fn determined(&self, mut done: Vec<bool>, keys: &[Vec<Key>], constant: bool) -> Vec<bool> {
        let n = self.items.len();
        let per_row: Vec<bool> = self.items.iter().map(|it| matches!(it.source, Source::Lateral(..))).collect();
        let varies = self.varying();
        loop {
            let mut changed = false;
            let steady: Vec<bool> = done.iter().zip(&varies).map(|(d, v)| *d && !v).collect();
            for i in 0..n {
                if done[i] {
                    continue;
                }
                let item = &self.items[i];
                let rest = || (0..n).all(|j| j == i || done[j] || (!constant && per_row[j]));
                let pinned = match &item.source {
                    Source::Table(_) => self.key_pinned(i, &keys[i], &steady, constant),
                    _ if constant && item.nullable => false,
                    Source::Query(q, ctes, enclosed) => at_most_one_in(q, ctes, *enclosed, self.keys).is_ok() || self.key_pinned(i, &keys[i], &steady, constant),
                    Source::Lateral(q, ctes) => rest() && (at_most_one_in(q, ctes, true, self.keys).is_ok() || self.key_pinned(i, &keys[i], &steady, constant)),
                    Source::Other => false,
                };
                if pinned {
                    done[i] = true;
                    changed = true;
                }
            }
            if !changed {
                return done;
            }
        }
    }

    /// Which items' columns may differ from row to row however few rows they have: the server may
    /// merge a derived table, a WITH query or a view into the query that reads it and call a
    /// function of it again for each row it meets, where it calls one whose every call may differ
    /// ([`calls_nondeterministic`]). Such an item pins no other, and its columns are no one value
    /// to group or to tell rows apart by.
    fn varying(&self) -> Vec<bool> {
        self.items
            .iter()
            .map(|it| match &it.source {
                Source::Query(q, ctes, _) => !materialized(q) && query_varies(q, ctes, self.keys, VIEW_DEPTH),
                Source::Lateral(q, ctes) => query_varies(q, ctes, self.keys, VIEW_DEPTH),
                _ => false,
            })
            .collect()
    }

    fn all_keys(&self) -> Vec<Vec<Key>> {
        (0..self.items.len()).map(|i| self.item_keys(i)).collect()
    }

    /// Whether every row of the FROM carries the same value of `e`: a column of a `constant` item,
    /// a column the WHERE equates to a value, or a value itself.
    fn pinned(&self, e: &Expr, constant: &[bool]) -> bool {
        match self.place_expr(e) {
            Some(Placed::Item(i, column)) => {
                // A varying item's column is no one value, a WHERE's equality with it included: a
                // merged derived table computes it again in the WHERE and in the select list.
                let varies = self.varying()[i];
                (constant[i] && !varies) || !varies && {
                    let class = self.class(i, &column);
                    let none = vec![false; self.items.len()];
                    self.conjuncts.iter().any(|c| c.may_pin.is_none() && self.equates(c.expr, i, &column, class, &none))
                }
            }
            Some(_) => false,
            None => !reads_column(e) && deterministic(e),
        }
    }

    /// Why the closure leaves the FROM more than one row, from its first undetermined item.
    fn why_not(&self, determined: &[bool]) -> String {
        let Some(i) = determined.iter().position(|d| !d) else { return "every table is pinned to one row".into() };
        let item = &self.items[i];
        match &item.source {
            Source::Table(t) => match self.keys.keys(t) {
                None => format!("`{t}` has no unique index the engine can read, so nothing bounds the rows a WHERE matches"),
                Some(keys) => {
                    let shown: Vec<String> = keys.iter().map(|k| format!("({})", k.join(", "))).collect();
                    if self.items.len() == 1 {
                        format!("the WHERE does not equate every column of a unique key of `{t}` — it has {}", shown.join(", "))
                    } else {
                        format!("a join returns a row per pair it matches, and the WHERE and ON clauses do not equate every column of a unique key of `{}` to values or to columns of tables pinned to one row — it has {}", item.alias, shown.join(", "))
                    }
                }
            },
            Source::Query(..) | Source::Lateral(..) => format!("`{}` returns any number of rows, and the WHERE does not equate every column of a unique key of its rows", item.alias),
            Source::Other => format!("`{}` in the FROM is a table function or a recursive WITH query, which returns any number of rows", item.alias),
        }
    }

    /// The output name of column `column` of item `i`, where the select list keeps it unchanged.
    fn output(&self, s: &Select, i: usize, column: &str) -> Option<String> {
        s.projection.iter().find_map(|item| match item {
            SelectItem::UnnamedExpr(e) => match self.place_expr(e)? {
                Placed::Item(j, c) if j == i && c == column => Some(c),
                _ => None,
            },
            SelectItem::ExprWithAlias { expr, alias } => match self.place_expr(expr)? {
                Placed::Item(j, c) if j == i && c == column => Some(alias.value.to_ascii_lowercase()),
                _ => None,
            },
            SelectItem::Wildcard(_) if !self.merged => Some(column.to_string()),
            SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) => match name.0.as_slice() {
                [ObjectNamePart::Identifier(q)] if self.named(&q.value) == Some(i) => Some(column.to_string()),
                _ => None,
            },
            _ => None,
        })
    }
}

/// Whether a statement's text calls a built-in whose every call may give another value: `RAND()`,
/// `UUID()`, `UUID_SHORT()`, `SYSDATE()`, `RANDOM_BYTES()`, and the counts of the statement's own
/// writes, `LAST_INSERT_ID()`, `ROW_COUNT()` and `FOUND_ROWS()`. `NOW()` reads the statement's
/// start, once.
fn calls_nondeterministic(text: &str) -> bool {
    const VARYING: &[&str] = &["rand", "uuid", "uuid_short", "sysdate", "random_bytes", "last_insert_id", "row_count", "found_rows"];
    let lower = text.to_ascii_lowercase();
    VARYING.iter().any(|name| {
        lower.match_indices(name).any(|(at, _)| {
            let before = lower[..at].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '`'));
            before && lower[at + name.len()..].trim_start().starts_with('(')
        })
    })
}

/// How many WITH queries and views deep [`query_varies`] reads before it takes a query to vary.
const VIEW_DEPTH: usize = 4;

/// Whether a query calls a function whose every call may differ ([`calls_nondeterministic`]), in
/// its own text, in a WITH query in `ctes` it may read, or in a view it reads, `depth` deep. A
/// WITH query of a RECURSIVE WITH, whose body the scope does not carry, may.
fn query_varies(q: &Query, ctes: &[Cte], keys: &Keys, depth: usize) -> bool {
    if calls_nondeterministic(&q.to_string()) || lists_unvouched(q) {
        return true;
    }
    // The names it reads as it parses them, a WITH query's around it among them, since it reads
    // those as it reads a table.
    let read = tables_anywhere(q, keys.schema.as_deref());
    let withs = ctes.iter().filter(|(name, ..)| read.iter().any(|t| crate::mysql_names::may_match(t, name))).any(|(_, body, sees, _)| depth == 0 || body.is_none_or(|b| query_varies(b, &ctes[..(*sees).min(ctes.len())], keys, depth - 1)));
    withs || read.iter().filter_map(|t| keys.view(t)).any(|view| depth == 0 || query_varies(view, &[], keys, depth - 1))
}

/// MySQL 8.4's built-in functions that give every call over the same arguments in one statement
/// one value, by name: its help tables' functions but its aggregates, window functions and those
/// [`calls_nondeterministic`] names, and the locking ones. A newer release's are unknown, and
/// [`calls_unvouched`] takes them to vary.
const BUILTINS: &[&str] = &[
    "abs", "acos", "adddate", "addtime", "aes_decrypt", "aes_encrypt", "any_value", "ascii", "asin", "atan", "atan2", "bin", "bin_to_uuid", "bit_count",
    "bit_length", "cast", "ceil", "ceiling", "char", "char_length", "character_length", "charset", "coalesce", "coercibility", "collation", "compress",
    "concat", "concat_ws", "conv", "convert", "convert_tz", "cos", "cot", "crc32", "curdate", "current_date", "current_role", "current_time",
    "current_timestamp", "current_user", "curtime", "database", "date", "date_add", "date_format", "date_sub", "datediff", "day", "dayname", "dayofmonth",
    "dayofweek", "dayofyear", "degrees", "div", "elt", "exp", "export_set", "extract", "field", "find_in_set", "floor", "format", "format_bytes",
    "format_pico_time", "from_base64", "from_days", "from_unixtime", "geometrycollection", "get_format", "greatest", "grouping", "hex", "hour",
    "icu_version", "if", "ifnull", "inet6_aton", "inet6_ntoa", "inet_aton", "inet_ntoa", "insert", "instr", "is_ipv4", "is_ipv4_compat", "is_ipv4_mapped",
    "is_ipv6", "is_uuid", "json_array", "json_array_append", "json_array_insert", "json_contains", "json_contains_path", "json_depth", "json_extract",
    "json_insert", "json_keys", "json_length", "json_merge", "json_merge_patch", "json_merge_preserve", "json_object", "json_overlaps", "json_pretty",
    "json_quote", "json_remove", "json_replace", "json_schema_valid", "json_schema_validation_report", "json_search", "json_set", "json_storage_free",
    "json_storage_size", "json_table", "json_type", "json_unquote", "json_valid", "json_value", "last_day", "lcase", "least", "left", "length", "like",
    "linestring", "ln", "load_file", "localtime", "localtimestamp", "locate", "log", "log10", "log2", "lower", "lpad", "ltrim", "make_set", "makedate",
    "maketime", "mbrcontains", "mbrcoveredby", "mbrcovers", "mbrdisjoint", "mbrequals", "mbrintersects", "mbroverlaps", "mbrtouches", "mbrwithin", "md5",
    "microsecond", "mid", "minute", "mod", "month", "monthname", "multilinestring", "multipoint", "multipolygon", "name_const", "now", "nullif", "oct",
    "octet_length", "ord", "period_add", "period_diff", "pi", "point", "polygon", "position", "pow", "power", "quarter", "quote", "radians", "regexp",
    "regexp_instr", "regexp_like", "regexp_replace", "regexp_substr", "repeat", "replace", "reverse", "right", "roles_graphml", "round", "rpad", "rtrim",
    "schema", "sec_to_time", "second", "session_user", "sha1", "sha2", "sign", "sin", "soundex", "space", "sqrt", "st_area", "st_asbinary", "st_asgeojson",
    "st_astext", "st_buffer", "st_buffer_strategy", "st_centroid", "st_collect", "st_contains", "st_convexhull", "st_crosses", "st_difference",
    "st_dimension", "st_disjoint", "st_distance", "st_distance_sphere", "st_endpoint", "st_envelope", "st_equals", "st_exteriorring", "st_frechetdistance",
    "st_geohash", "st_geomcollfromtext", "st_geomcollfromwkb", "st_geometryn", "st_geometrytype", "st_geomfromgeojson", "st_geomfromtext",
    "st_geomfromwkb", "st_hausdorffdistance", "st_interiorringn", "st_intersection", "st_intersects", "st_isclosed", "st_isempty", "st_issimple",
    "st_isvalid", "st_latfromgeohash", "st_latitude", "st_length", "st_linefromtext", "st_linefromwkb", "st_lineinterpolatepoint",
    "st_lineinterpolatepoints", "st_longfromgeohash", "st_longitude", "st_makeenvelope", "st_mlinefromtext", "st_mlinefromwkb", "st_mpointfromtext",
    "st_mpointfromwkb", "st_mpolyfromtext", "st_mpolyfromwkb", "st_numgeometries", "st_numinteriorrings", "st_numpoints", "st_overlaps",
    "st_pointatdistance", "st_pointfromgeohash", "st_pointfromtext", "st_pointfromwkb", "st_pointn", "st_polyfromtext", "st_polyfromwkb", "st_simplify",
    "st_srid", "st_startpoint", "st_swapxy", "st_symdifference", "st_touches", "st_transform", "st_union", "st_validate", "st_within", "st_x", "st_y",
    "str_to_date", "strcmp", "subdate", "substr", "substring", "substring_index", "subtime", "system_user", "tan", "time", "time_format", "time_to_sec",
    "timediff", "timestamp", "timestampadd", "timestampdiff", "to_base64", "to_days", "to_seconds", "trim", "truncate", "ucase", "uncompress",
    "uncompressed_length", "unhex", "unix_timestamp", "upper", "user", "utc_date", "utc_time", "utc_timestamp", "uuid_to_bin", "version", "week",
    "weekday", "weekofyear", "weight_string", "year", "yearweek",
];

/// Whether an expression calls a function the engine cannot vouch gives every call one value:
/// neither a built-in ([`BUILTINS`]) nor an aggregate, as a stored function the database declares
/// `NOT DETERMINISTIC` may be; or holds a form the walk does not read. A select list that calls
/// one varies ([`query_varies`]).
fn calls_unvouched(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_) | Expr::TypedString { .. } => false,
        Expr::Function(f) => {
            let name = builtin(f);
            let vouched = f.over.is_some() || crate::mysql::AGGREGATES.contains(&name.as_str()) || BUILTINS.contains(&name.as_str());
            !vouched
                || match &f.args {
                    ast::FunctionArguments::None => false,
                    ast::FunctionArguments::List(list) => list.args.iter().any(|a| match a {
                        ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(x)) | ast::FunctionArg::Named { arg: ast::FunctionArgExpr::Expr(x), .. } | ast::FunctionArg::ExprNamed { arg: ast::FunctionArgExpr::Expr(x), .. } => calls_unvouched(x),
                        ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard) => false,
                        _ => true,
                    }),
                    ast::FunctionArguments::Subquery(q) => lists_unvouched(q),
                }
        }
        Expr::Nested(inner)
        | Expr::UnaryOp { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull(inner)
        | Expr::IsNotNull(inner)
        | Expr::IsTrue(inner)
        | Expr::IsNotTrue(inner)
        | Expr::IsFalse(inner)
        | Expr::IsNotFalse(inner)
        | Expr::IsUnknown(inner)
        | Expr::IsNotUnknown(inner)
        | Expr::Floor { expr: inner, .. }
        | Expr::Ceil { expr: inner, .. }
        | Expr::Extract { expr: inner, .. }
        | Expr::Convert { expr: inner, .. }
        | Expr::Collate { expr: inner, .. } => calls_unvouched(inner),
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom(left, right) | Expr::IsNotDistinctFrom(left, right) | Expr::Position { expr: left, r#in: right } => calls_unvouched(left) || calls_unvouched(right),
        Expr::Like { expr, pattern, escape_char, .. } => calls_unvouched(expr) || calls_unvouched(pattern) || escape_char.as_deref().is_some_and(calls_unvouched),
        Expr::RLike { expr, pattern, .. } => calls_unvouched(expr) || calls_unvouched(pattern),
        Expr::Substring { expr, substring_from, substring_for, .. } => calls_unvouched(expr) || substring_from.as_deref().is_some_and(calls_unvouched) || substring_for.as_deref().is_some_and(calls_unvouched),
        Expr::Trim { expr, trim_what, trim_characters, .. } => calls_unvouched(expr) || trim_what.as_deref().is_some_and(calls_unvouched) || trim_characters.iter().flatten().any(calls_unvouched),
        Expr::InList { expr, list, .. } => calls_unvouched(expr) || list.iter().any(calls_unvouched),
        Expr::Between { expr, low, high, .. } => calls_unvouched(expr) || calls_unvouched(low) || calls_unvouched(high),
        Expr::Case { operand, conditions, else_result, .. } => operand.as_deref().is_some_and(calls_unvouched) || conditions.iter().any(|c| calls_unvouched(&c.condition) || calls_unvouched(&c.result)) || else_result.as_deref().is_some_and(calls_unvouched),
        Expr::Interval(i) => calls_unvouched(&i.value),
        Expr::MemberOf(m) => calls_unvouched(&m.value) || calls_unvouched(&m.array),
        Expr::Tuple(items) => items.iter().any(calls_unvouched),
        // The server folds a subquery of no table into the query around it (`(select pick())` is
        // `pick()`, `pick() in (select 1)` is `pick() = 1`), and then merges as if it were not there.
        Expr::Subquery(q) => lists_unvouched(q),
        // EXISTS reads no value of its subquery's select list, only whether a row passes, unless a
        // HAVING reads the list by an alias.
        Expr::Exists { subquery, .. } => reads_unvouched(subquery, plain(subquery).is_none_or(|s| s.having.is_some())),
        Expr::InSubquery { expr, subquery, .. } => calls_unvouched(expr) || lists_unvouched(subquery),
        Expr::MatchAgainst { .. } => false,
        _ => true,
    }
}

/// Whether the server computes a derived table, WITH query or view once and stores its rows,
/// never merging it into the query that reads it, so that every row reads one value of each of its
/// columns: one with a LIMIT, a set operation, DISTINCT, GROUP BY, HAVING or an aggregate, or no
/// table (https://dev.mysql.com/doc/refman/8.4/en/derived-table-optimization.html). A LATERAL one
/// is computed again for each row it is joined to.
fn materialized(q: &Query) -> bool {
    if q.limit_clause.is_some() {
        return true;
    }
    match q.body.as_ref() {
        SetExpr::Query(inner) => materialized(inner),
        SetExpr::Select(s) => {
            let grouped = !matches!(&s.group_by, GroupByExpr::Expressions(keys, modifiers) if keys.is_empty() && modifiers.is_empty());
            s.from.is_empty() || s.distinct.is_some() || grouped || s.having.is_some() || crate::mysql::lone_aggregate(s)
        }
        _ => true,
    }
}

/// Whether a query, or a derived table or WITH query inside it, lists a value [`calls_unvouched`]
/// makes, or decides which rows it keeps by one: the server may merge each level into the one
/// around it.
fn lists_unvouched(q: &Query) -> bool {
    reads_unvouched(q, true)
}

/// [`lists_unvouched`], reading the select list only where `listed`: EXISTS reads no value of it.
fn reads_unvouched(q: &Query, listed: bool) -> bool {
    if q.with.iter().flat_map(|w| &w.cte_tables).any(|c| lists_unvouched(&c.query)) {
        return true;
    }
    if let SetExpr::Query(inner) = q.body.as_ref() {
        return reads_unvouched(inner, listed);
    }
    fn factor(f: &TableFactor) -> bool {
        match f {
            TableFactor::Derived { subquery, .. } => lists_unvouched(subquery),
            TableFactor::NestedJoin { table_with_joins, .. } => factor(&table_with_joins.relation) || table_with_joins.joins.iter().any(|j| factor(&j.relation)),
            _ => false,
        }
    }
    plain(q).is_some_and(|s| {
        (listed && s.projection.iter().any(|item| matches!(item, SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } if calls_unvouched(e))))
            || s.selection.iter().chain(s.having.iter()).any(calls_unvouched)
            || s.from.iter().any(|twj| factor(&twj.relation) || twj.joins.iter().any(|j| factor(&j.relation)))
    })
}

/// The terms of a condition's top-level `AND`.
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::BinaryOp { left, op: ast::BinaryOperator::And, right } => {
            let mut out = conjuncts(left);
            out.extend(conjuncts(right));
            out
        }
        Expr::Nested(inner) => conjuncts(inner),
        other => vec![other],
    }
}

/// The SELECT a query's body is, when it is one.
fn plain(q: &Query) -> Option<&Select> {
    match q.body.as_ref() {
        SetExpr::Select(s) => Some(s),
        _ => None,
    }
}

/// `inner` with the names of `outer` over them, position by position.
fn overlay(inner: &[String], outer: &[String]) -> Vec<String> {
    let mut columns = inner.to_vec();
    for (at, name) in outer.iter().enumerate() {
        match columns.get_mut(at) {
            Some(slot) => *slot = name.clone(),
            None => columns.push(name.clone()),
        }
    }
    columns
}

/// A query's output names after a column list renames them by position; `None` where the list
/// does not name every column, which the server refuses.
fn renamed_names(names: Vec<String>, columns: &[String]) -> Option<Vec<String>> {
    if columns.is_empty() {
        return Some(names);
    }
    (columns.len() == names.len()).then(|| columns.to_vec())
}

/// The query's own name for the column a column list calls `column`.
fn inner_name(column: &str, columns: &[String], names: impl FnOnce() -> Option<Vec<String>>) -> Option<String> {
    if columns.is_empty() {
        return Some(column.to_string());
    }
    let names = names()?;
    let at = columns.iter().position(|c| c == column)?;
    (columns.len() == names.len()).then(|| names[at].clone())
}

/// `keys` of a query's rows, each column by the name a column list gives it; a key whose column the
/// query's own names do not tell apart is dropped.
fn renamed_keys(keys: Vec<Key>, columns: &[String], names: impl FnOnce() -> Option<Vec<String>>) -> Vec<Key> {
    if columns.is_empty() {
        return keys;
    }
    let Some(names) = names().filter(|n| n.len() == columns.len()) else { return Vec::new() };
    let at = |name: &str| {
        let mut found = names.iter().enumerate().filter(|(_, n)| *n == name).map(|(i, _)| i);
        match (found.next(), found.next()) {
            (Some(i), None) => Some(i),
            _ => None,
        }
    };
    keys.into_iter().filter_map(|key| key.into_iter().map(|p| Some(Part { name: columns[at(&p.name)?].clone(), class: p.class })).collect()).collect()
}

/// The names of a query's output columns, in lower case, where the engine can tell them all.
fn output_names(q: &Query, outer: &[Cte], enclosed: bool, keys: &Keys) -> Option<Vec<String>> {
    let s = plain(q)?;
    let ctes = ctes_of(q, outer);
    let from = From::read(s, &ctes, enclosed, keys);
    let mut out = Vec::new();
    for item in &s.projection {
        match item {
            SelectItem::UnnamedExpr(Expr::Identifier(i)) => out.push(i.value.to_ascii_lowercase()),
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => out.push(parts.last()?.value.to_ascii_lowercase()),
            SelectItem::ExprWithAlias { alias, .. } => out.push(alias.value.to_ascii_lowercase()),
            SelectItem::Wildcard(_) if !from.merged && from.refused.is_none() => {
                for i in 0..from.items.len() {
                    out.extend(from.columns(i)?);
                }
            }
            SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) => {
                let [ObjectNamePart::Identifier(q)] = name.0.as_slice() else { return None };
                let i = from.named(&q.value)?;
                out.extend(from.columns(i)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// What output column `name` of a query compares as: that of the column it passes through.
fn output_class(q: &Query, outer: &[Cte], enclosed: bool, keys: &Keys, name: &str) -> Option<Class> {
    let s = plain(q)?;
    let ctes = ctes_of(q, outer);
    let from = From::read(s, &ctes, enclosed, keys);
    if from.refused.is_some() {
        return None;
    }
    s.projection.iter().find_map(|item| {
        let (expr, out) = match item {
            SelectItem::UnnamedExpr(e) => (e, None),
            SelectItem::ExprWithAlias { expr, alias } => (expr, Some(alias.value.to_ascii_lowercase())),
            SelectItem::Wildcard(_) if !from.merged => {
                return (0..from.items.len()).find(|i| from.columns(*i).is_some_and(|c| c.iter().any(|c| c == name))).and_then(|i| from.class(i, name));
            }
            _ => return None,
        };
        match from.place_expr(expr)? {
            Placed::Item(i, c) if out.as_deref().unwrap_or(&c) == name => from.class(i, &c),
            _ => None,
        }
    })
}

/// The table columns that output column `name` (lower case) of a query passes through unchanged,
/// each table by the name the query writes it, read without the catalog: a derived table or a WITH
/// query through its select list, down to a table, and a set operation through each of its sides
/// (one column each). `columns` is a column list that renames the query's columns; `outer` are the
/// WITH queries it sees ([`ctes_of`]), so that none of their names is read as a table. Empty where
/// any side gives none.
/// `enclosed` where the query sees the FROM items of an enclosing query ([`From::enclosed`]).
pub(crate) fn output_origins(q: &Query, outer: &[Cte], enclosed: bool, columns: &[String], name: &str) -> Vec<(String, String)> {
    let keys = Keys::default();
    let Some(inner) = inner_name(name, columns, || output_names(q, outer, enclosed, &keys)) else { return Vec::new() };
    body_origins(&q.body, &ctes_of(q, outer), enclosed, &keys, &inner).unwrap_or_default()
}

fn body_origins<'a>(body: &'a SetExpr, ctes: &[Cte<'a>], enclosed: bool, keys: &'a Keys, name: &str) -> Option<Vec<(String, String)>> {
    match body {
        SetExpr::Select(s) => select_origins(s, ctes, enclosed, keys, name),
        SetExpr::Query(q) => body_origins(&q.body, &ctes_of(q, ctes), enclosed, keys, name),
        SetExpr::SetOperation { left, right, .. } => {
            let at = body_columns(left, ctes, enclosed, keys)?.iter().position(|(n, _)| n == name)?;
            let other = body_columns(right, ctes, enclosed, keys)?.get(at)?.0.clone();
            let mut out = body_origins(left, ctes, enclosed, keys, name)?;
            out.extend(body_origins(right, ctes, enclosed, keys, &other)?);
            Some(out)
        }
        _ => None,
    }
}

fn select_origins<'a>(s: &'a Select, ctes: &[Cte<'a>], enclosed: bool, keys: &'a Keys, name: &str) -> Option<Vec<(String, String)>> {
    let from = From::read(s, ctes, enclosed, keys);
    if from.refused.is_some() {
        return None;
    }
    let through = |i: usize, column: &str| -> Option<Vec<(String, String)>> {
        if from.items[i].renamed {
            return None;
        }
        match &from.items[i].source {
            Source::Table(t) => Some(vec![(t.clone(), column.to_string())]),
            Source::Query(q, ctes, enclosed) => body_origins(&q.body, &ctes_of(q, ctes), *enclosed, keys, &inner_name(column, &from.items[i].columns, || output_names(q, ctes, *enclosed, keys))?),
            _ => None,
        }
    };
    s.projection.iter().find_map(|item| match item {
        SelectItem::UnnamedExpr(e) => match from.place_expr(e)? {
            Placed::Item(i, c) if c == name => through(i, &c),
            _ => None,
        },
        SelectItem::ExprWithAlias { expr, alias } if alias.value.eq_ignore_ascii_case(name) => match from.place_expr(expr)? {
            Placed::Item(i, c) => through(i, &c),
            _ => None,
        },
        SelectItem::Wildcard(_) if !from.merged && from.items.len() == 1 => through(0, name),
        SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(n), _) => match n.0.as_slice() {
            [ObjectNamePart::Identifier(q)] => from.named(&q.value).and_then(|i| through(i, name)),
            _ => None,
        },
        _ => None,
    })
}

/// The unique keys of a query's rows: sets of its output columns no two of its rows share a value
/// of every one of (NULL aside, which no `=` matches).
///
/// - A unique key of one of its FROM items, every column of it in the select list unchanged, when
///   every other item is determined against that item: each of its rows then meets one row at
///   most of the rest. A WHERE, a HAVING, a LIMIT and an OFFSET only remove rows, and a window
///   function adds none. A LATERAL derived table is no such item: its rows are computed anew for
///   each row of the rest, so two of them may share a key.
/// - The GROUP BY's expressions, each a column of the select list; not WITH ROLLUP, which adds a
///   row per level.
/// - Under DISTINCT, the whole select list, every entry a column.
///
/// - A set operation's ([`set_keys`]).
///
/// None for a VALUES list.
fn output_keys(q: &Query, outer: &[Cte], enclosed: bool, keys: &Keys) -> Vec<Key> {
    found(|found| &mut found.keys, q, outer, enclosed, || {
        let ctes = ctes_of(q, outer);
        body_keys(&q.body, &ctes, enclosed, keys)
    })
}

/// [`output_keys`] of a query's body, with the WITH queries it sees.
fn body_keys<'a>(body: &'a SetExpr, ctes: &[Cte<'a>], enclosed: bool, keys: &'a Keys) -> Vec<Key> {
    let s = match body {
        SetExpr::Select(s) => s,
        SetExpr::Query(q) => return output_keys(q, ctes, enclosed, keys),
        SetExpr::SetOperation { op, set_quantifier, left, right } => return set_keys(op, set_quantifier, left, right, ctes, enclosed, keys),
        _ => return Vec::new(),
    };
    let from = From::read(s, ctes, enclosed, keys);
    if from.refused.is_some() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // The output column a GROUP BY expression is: a column, a column by its position, or an alias.
    let part = |g: &Expr| -> Option<Part> {
        let placed = match g {
            Expr::Value(v) => match &v.value {
                ast::Value::Number(..) => from.place_expr(grouped_by(s, g)?)?,
                _ => return None,
            },
            // A bare name is a column of the FROM first, and else an alias of the select list.
            _ => match from.place_expr(g)? {
                Placed::Item(i, c) => Placed::Item(i, c),
                _ => {
                    let Expr::Identifier(name) = g else { return None };
                    let aliased = s.projection.iter().find_map(|item| match item {
                        SelectItem::ExprWithAlias { expr, alias } if alias.value.eq_ignore_ascii_case(&name.value) => Some(expr),
                        _ => None,
                    })?;
                    from.place_expr(aliased)?
                }
            },
        };
        let Placed::Item(i, c) = placed else { return None };
        Some(Part { name: from.output(s, i, &c)?, class: from.class(i, &c) })
    };
    match &s.group_by {
        GroupByExpr::Expressions(exprs, modifiers) if !exprs.is_empty() => {
            if modifiers.is_empty() {
                out.extend(exprs.iter().map(part).collect::<Option<Key>>());
            }
            return out;
        }
        GroupByExpr::All(_) => return out,
        _ => {}
    }
    if matches!(s.distinct, Some(Distinct::Distinct)) {
        let key: Option<Key> = s
            .projection
            .iter()
            .map(|item| {
                let (e, alias) = match item {
                    SelectItem::UnnamedExpr(e) => (e, None),
                    SelectItem::ExprWithAlias { expr, alias } => (expr, Some(alias.value.to_ascii_lowercase())),
                    _ => return None,
                };
                let Placed::Item(i, c) = from.place_expr(e)? else { return None };
                Some(Part { name: alias.unwrap_or_else(|| c.clone()), class: from.class(i, &c) })
            })
            .collect();
        out.extend(key);
    }
    let all = from.all_keys();
    let n = from.items.len();
    for r in 0..n {
        if !matches!(from.items[r].source, Source::Table(_) | Source::Query(..)) {
            continue;
        }
        let mut seed = vec![false; n];
        seed[r] = true;
        if !from.determined(seed, &all, false).iter().all(|d| *d) {
            continue;
        }
        // A key column of a table the select list leaves out still counts where the WHERE pins
        // it to one value.
        let fixed = |p: &Part| matches!(from.items[r].source, Source::Table(_)) && from.conjuncts.iter().any(|c| c.may_pin.is_none() && from.equates(c.expr, r, &p.name, p.class, &vec![false; n]));
        for key in &all[r] {
            out.extend(
                key.iter()
                    .filter_map(|p| match from.output(s, r, &p.name) {
                        Some(name) => Some(Some(Part { name, class: p.class })),
                        None if fixed(p) => None,
                        None => Some(None),
                    })
                    .collect::<Option<Key>>(),
            );
        }
    }
    out
}

/// The keys of a set operation's rows. Without ALL every operator returns distinct rows, so the
/// whole row is a key, where every column is an exact number on both sides, which compare alike
/// however the server joins their types. INTERSECT returns only rows of its left side, and EXCEPT a
/// subset of them, with ALL or without, so those keep its keys. UNION ALL has none.
fn set_keys<'a>(op: &ast::SetOperator, quantifier: &ast::SetQuantifier, left: &'a SetExpr, right: &'a SetExpr, ctes: &[Cte<'a>], enclosed: bool, keys: &'a Keys) -> Vec<Key> {
    let mut out = Vec::new();
    if matches!(op, ast::SetOperator::Intersect | ast::SetOperator::Except) {
        out.extend(body_keys(left, ctes, enclosed, keys));
    }
    if !matches!(quantifier, ast::SetQuantifier::All | ast::SetQuantifier::AllByName) {
        if let (Some(l), Some(r)) = (body_columns(left, ctes, enclosed, keys), body_columns(right, ctes, enclosed, keys)) {
            let exact = !l.is_empty() && l.len() == r.len() && l.iter().zip(&r).all(|((_, a), (_, b))| a.is_some_and(Class::exact) && b.is_some_and(Class::exact));
            if exact {
                out.push(l.into_iter().map(|(name, class)| Part { name, class }).collect());
            }
        }
    }
    out
}

/// The output columns of a query's body by position, each with its name in lower case and what
/// it compares as where it is a column; `None` where the engine cannot name them all. A set
/// operation's are its left side's names, and a class both sides share.
fn body_columns<'a>(body: &'a SetExpr, ctes: &[Cte<'a>], enclosed: bool, keys: &'a Keys) -> Option<Vec<(String, Option<Class>)>> {
    match body {
        SetExpr::Select(s) => {
            let from = From::read(s, ctes, enclosed, keys);
            if from.refused.is_some() {
                return None;
            }
            let class = |e: &Expr| match from.place_expr(e) {
                Some(Placed::Item(i, c)) => from.class(i, &c),
                _ => None,
            };
            let mut out = Vec::new();
            for item in &s.projection {
                match item {
                    SelectItem::UnnamedExpr(e @ Expr::Identifier(i)) => out.push((i.value.to_ascii_lowercase(), class(e))),
                    SelectItem::UnnamedExpr(e @ Expr::CompoundIdentifier(parts)) => out.push((parts.last()?.value.to_ascii_lowercase(), class(e))),
                    SelectItem::ExprWithAlias { expr, alias } => out.push((alias.value.to_ascii_lowercase(), class(expr))),
                    SelectItem::Wildcard(_) if !from.merged => {
                        for i in 0..from.items.len() {
                            for c in from.columns(i)? {
                                let k = from.class(i, &c);
                                out.push((c, k));
                            }
                        }
                    }
                    _ => return None,
                }
            }
            Some(out)
        }
        SetExpr::Query(q) => body_columns(&q.body, &ctes_of(q, ctes), enclosed, keys),
        SetExpr::SetOperation { left, right, .. } => {
            let (l, r) = (body_columns(left, ctes, enclosed, keys)?, body_columns(right, ctes, enclosed, keys)?);
            (l.len() == r.len()).then(|| l.into_iter().zip(r).map(|((name, a), (_, b))| (name, if a == b { a } else { None })).collect())
        }
        _ => None,
    }
}

/// Prove that a query returns at most one row, or say why the engine cannot.
pub(crate) fn at_most_one(q: &Query, keys: &Keys) -> Result<(), String> {
    struct Outer(Option<Found>);
    impl Drop for Outer {
        fn drop(&mut self) {
            let outer = self.0.take();
            FOUND.with(|found| *found.borrow_mut() = outer);
        }
    }
    let _outer = Outer(FOUND.with(|found| found.borrow_mut().replace(Found::default())));
    at_most_one_in(q, &[], false, keys)
}

/// What [`at_most_one_in`] and [`output_keys`] found of each query while one [`at_most_one`] runs,
/// by the query and the WITH queries it sees. A FROM asks both of each derived table and WITH
/// query it reads, and asks again for each item it seeds a closure with, so answered anew each
/// time, the work would double with each level of nesting. Every query asked about lives as long
/// as the call: in the statement's tree, or among the views of [`Keys`].
#[derive(Default)]
struct Found {
    at_most_one: HashMap<Asked, Result<(), String>>,
    keys: HashMap<Asked, Vec<Key>>,
}

/// A query, by its place, whether it sees an enclosing query's FROM items, and the WITH queries it
/// sees, each by its name, its body's place, how many before it its body sees, and its column list.
type Asked = (usize, bool, Vec<(String, Option<usize>, usize, Vec<String>)>);

thread_local! {
    static FOUND: RefCell<Option<Found>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// How many queries [`found`] proved anew.
    pub(crate) static FINDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What `find` answers for `q` under `outer`, found once while an [`at_most_one`] runs, and anew
/// outside one. `table` is where the answers of `find`'s kind are kept.
fn found<T: Clone>(table: fn(&mut Found) -> &mut HashMap<Asked, T>, q: &Query, outer: &[Cte], enclosed: bool, find: impl FnOnce() -> T) -> T {
    let running = FOUND.with(|found| found.borrow().is_some());
    if !running {
        return find();
    }
    let asked: Asked = (q as *const Query as usize, enclosed, outer.iter().map(|(name, body, sees, columns)| (name.clone(), body.map(|b| b as *const Query as usize), *sees, columns.clone())).collect());
    if let Some(answer) = FOUND.with(|found| found.borrow_mut().as_mut().and_then(|found| table(found).get(&asked).cloned())) {
        return answer;
    }
    #[cfg(test)]
    FINDS.set(FINDS.get() + 1);
    let answer = find();
    FOUND.with(|found| {
        if let Some(found) = found.borrow_mut().as_mut() {
            table(found).insert(asked, answer.clone());
        }
    });
    answer
}

fn at_most_one_in(q: &Query, outer: &[Cte], enclosed: bool, keys: &Keys) -> Result<(), String> {
    found(|found| &mut found.at_most_one, q, outer, enclosed, || at_most_one_anew(q, outer, enclosed, keys))
}

fn at_most_one_anew(q: &Query, outer: &[Cte], enclosed: bool, keys: &Keys) -> Result<(), String> {
    if matches!(const_limit(q), Some(Some(n)) if (0..=1).contains(&n)) {
        return Ok(());
    }
    // Any other LIMIT, and an OFFSET, only remove rows: the bound is the one the statement has
    // without them.
    if one_before_limit(q).is_ok() {
        return Ok(());
    }
    let s = select_of(q)?;
    // With no FROM, or FROM DUAL, there is one row for the WHERE, a GROUP BY or a HAVING to keep or
    // remove; WITH ROLLUP adds a row for each level of the grouping.
    if (s.from.is_empty() || is_dual(s)) && matches!(&s.group_by, GroupByExpr::Expressions(_, modifiers) if modifiers.is_empty()) {
        return Ok(());
    }
    let ctes = ctes_of(q, outer);
    let from = From::read(s, &ctes, enclosed, keys);
    if let Some(why) = from.refused {
        return Err(why.into());
    }
    let all = from.all_keys();
    let n = from.items.len();
    let grouped: Option<&[Expr]> = match &s.group_by {
        GroupByExpr::Expressions(_, modifiers) if !modifiers.is_empty() => return Err("GROUP BY … WITH ROLLUP adds a row for every level of the grouping".into()),
        GroupByExpr::Expressions(e, _) if !e.is_empty() => Some(e),
        GroupByExpr::All(_) => return Err("GROUP BY ALL returns one row per group".into()),
        _ => None,
    };
    if let Some(group) = grouped {
        if from.determined(vec![false; n], &all, false).iter().all(|d| *d) {
            return Ok(());
        }
        let constant = from.determined(vec![false; n], &all, true);
        if group.iter().all(|g| grouped_by(s, g).is_some_and(|e| from.pinned(e, &constant))) {
            return Ok(());
        }
        return Err("GROUP BY returns one row per group; the engine proves one group only when every GROUP BY expression is pinned to one value: a column of a table whose unique key the WHERE pins, or a column the WHERE equates to a value".into());
    }
    if s.having.is_some() {
        return Err("HAVING leaves a row per group".into());
    }
    let determined = from.determined(vec![false; n], &all, false);
    if determined.iter().all(|d| *d) {
        return Ok(());
    }
    if matches!(s.distinct, Some(Distinct::Distinct)) {
        let constant = from.determined(vec![false; n], &all, true);
        let pinned = s.projection.iter().all(|item| match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => from.pinned(e, &constant),
            _ => false,
        });
        if pinned {
            return Ok(());
        }
    }
    Err(from.why_not(&determined))
}

/// What a GROUP BY expression groups by: MySQL reads a whole number as the select list's item at
/// that position, never as the constant. None for a position past the list or at a wildcard.
fn grouped_by<'a>(s: &'a Select, g: &'a Expr) -> Option<&'a Expr> {
    let Expr::Value(v) = g else { return Some(g) };
    let ast::Value::Number(n, _) = &v.value else { return Some(g) };
    match s.projection.get(n.parse::<usize>().ok()?.checked_sub(1)?)? {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => Some(e),
        _ => None,
    }
}

/// Every table a query reads anywhere in its FROM trees, as written: every name but one that each
/// server reads as a WITH query ([`crate::mysql_names::with_query`]).
pub(crate) fn tables_anywhere(q: &Query, schema: Option<&str>) -> Vec<String> {
    fn query(q: &Query, ctes: &mut Vec<String>, schema: Option<&str>, out: &mut Vec<String>) {
        let depth = ctes.len();
        if let Some(w) = &q.with {
            for c in &w.cte_tables {
                // A WITH query's body sees the ones before it, and with RECURSIVE itself too: a
                // name it reads otherwise is a table, or a WITH query of an enclosing query.
                if w.recursive {
                    ctes.push(c.alias.name.value.clone());
                    query(&c.query, ctes, schema, out);
                } else {
                    query(&c.query, ctes, schema, out);
                    ctes.push(c.alias.name.value.clone());
                }
            }
        }
        body(&q.body, ctes, schema, out);
        ctes.truncate(depth);
    }
    fn body(b: &SetExpr, ctes: &mut Vec<String>, schema: Option<&str>, out: &mut Vec<String>) {
        match b {
            SetExpr::Select(s) => {
                for twj in &s.from {
                    factor(&twj.relation, ctes, schema, out);
                    for j in &twj.joins {
                        factor(&j.relation, ctes, schema, out);
                    }
                }
                // A subquery in the WHERE, the select list or the HAVING, whose keys and columns
                // say whether it is one value a key can be pinned to.
                let mut found = Vec::new();
                for e in s.selection.iter().chain(s.having.iter()).chain(s.projection.iter().filter_map(|i| match i {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => Some(e),
                    _ => None,
                })) {
                    crate::mysql::subqueries(e, &mut found);
                }
                for sub in found {
                    query(sub, ctes, schema, out);
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                body(left, ctes, schema, out);
                body(right, ctes, schema, out);
            }
            SetExpr::Query(q) => query(q, ctes, schema, out),
            _ => {}
        }
    }
    fn factor(f: &TableFactor, ctes: &mut Vec<String>, schema: Option<&str>, out: &mut Vec<String>) {
        match f {
            TableFactor::Table { name, args: None, .. } => match name.0.as_slice() {
                [ObjectNamePart::Identifier(t)] if !matches!(crate::mysql_names::with_query(ctes.iter().map(String::as_str), &t.value), Reads::With(_)) => out.push(t.value.clone()),
                // The view's own schema as it is spelled ([`From::factor`]).
                [ObjectNamePart::Identifier(s), ObjectNamePart::Identifier(t)] if schema == Some(s.value.as_str()) => out.push(t.value.clone()),
                _ => {}
            },
            TableFactor::Derived { subquery, .. } => query(subquery, ctes, schema, out),
            TableFactor::NestedJoin { table_with_joins, .. } => {
                factor(&table_with_joins.relation, ctes, schema, out);
                for j in &table_with_joins.joins {
                    factor(&j.relation, ctes, schema, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    query(q, &mut Vec::new(), schema, &mut out);
    out.sort();
    out.dedup();
    out
}
