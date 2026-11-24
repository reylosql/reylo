//! How many rows a statement returns, as far as ClickHouse's own query tree says (`EXPLAIN QUERY
//! TREE`, a line a node, two spaces a level). ClickHouse enforces no unique key, a primary key
//! being a sort key, so no `WHERE id = …` bounds a result. Two things do:
//!
//! - `LIMIT 1`, written as a number, which the tree gives as a constant with nothing under it: at
//!   most one row. A limit the server folded from an expression is none the statement fixes: a
//!   parameter's stand-in is one, and so is a subquery's answer, computed as the tree is built.
//!   `WITH TIES` lifts the bound, and a set operation's root is no query.
//! - An aggregate with no `GROUP BY`: one row, whatever it reads, an empty input included. The
//!   tree is what tells it from an aggregate grouped by nothing (`GROUP BY ()`) or by a constant,
//!   which returns no row over an empty input, and from a view that aggregates, which the tree
//!   leaves a table. An `arrayJoin` after the aggregation makes several rows of the one, and so
//!   does a `WITH FILL`, so neither is bounded. A `HAVING`, a `QUALIFY`, an `OFFSET`, a `LIMIT`
//!   or a `LIMIT BY` may drop the row, and so may a `SETTINGS` clause that names a setting which
//!   drops rows (`empty_result_for_aggregation_by_empty_set`, `limit`, `offset`,
//!   `additional_result_filter`): at most one row. One that names `distributed_group_by_no_merge`
//!   leaves each shard's rows unmerged, and bounds nothing.
//!
//! The tree's text is the server's to change, so what is not recognized proves nothing.
//!
//! The tree also says which columns of a SELECT hold values that depend on a parameter
//! ([`flows`]), which a write is held by: there, what is not recognized depends on one.

use std::collections::{HashMap, HashSet};

/// What a column's values depend on, of the statement's parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flow {
    /// The type of each parameter the values are computed from, as its stand-in has it.
    pub of: Vec<String>,
    /// Whether they may be computed from one whose type the tree does not say: a source the
    /// engine does not know, a table function's rows, a row a WITH FILL adds.
    pub unknown: bool,
}

impl Flow {
    /// Whether the values depend on a parameter at all.
    pub fn depends(&self) -> bool {
        self.unknown || !self.of.is_empty()
    }
}

/// One of the values a choice between values may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A text the statement writes, with no escape in it.
    Text(String),
    /// A NULL the statement writes.
    Null,
    /// Any value of a type, by the type's name: a column's, or what a function computes.
    Typed(String),
}

/// What a query tree proves of the statement's row count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rows {
    /// The statement's own `LIMIT` is the constant 1 or 0, with no ties, or the statement is an
    /// aggregate with no `GROUP BY` whose row nothing multiplies.
    pub at_most_one: bool,
    /// The statement is one aggregate with no `GROUP BY`, and nothing after it drops or adds a row.
    pub one: bool,
    /// The statement is an aggregate with no `GROUP BY` whose row nothing multiplies: what bounds it.
    pub aggregate: bool,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The top query's sections: each one's heading, and the lines under it.
fn sections<'a>(lines: &[&'a str]) -> Vec<(&'a str, Vec<&'a str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in &lines[1..] {
        match indent(line) {
            2 => out.push((line.trim(), Vec::new())),
            n if n > 2 => {
                if let Some(last) = out.last_mut() {
                    last.1.push(*line);
                }
            }
            _ => {}
        }
    }
    out
}

/// The lines of a section that are the top query's own: those of a subquery inside it left out.
fn own<'a>(body: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut inside: Option<usize> = None;
    for line in body {
        if let Some(depth) = inside {
            if indent(line) > depth {
                continue;
            }
            inside = None;
        }
        let node = line.trim_start();
        if node.starts_with("QUERY id:") || node.starts_with("UNION id:") {
            inside = Some(indent(line));
            continue;
        }
        out.push(*line);
    }
    out
}

/// The whole number a `CONSTANT` node holds, as `constant_value: UInt64_1` writes it.
fn constant(line: &str) -> Option<u64> {
    let node = line.trim_start();
    if !node.starts_with("CONSTANT ") {
        return None;
    }
    let value = node.split("constant_value: ").nth(1)?.split(',').next()?;
    let (ty, digits) = value.rsplit_once('_')?;
    (ty.starts_with("UInt") || ty.starts_with("Int")).then(|| digits.parse().ok()).flatten()
}

/// Why a statement's result is one Reylo does not read whole, where the tree shows it: the
/// totals of `WITH TOTALS` come beside the rows, in a part of the result that the rows' format
/// leaves out, and a `SETTINGS` clause of the statement's own that changes how a result is
/// written outranks what the run time asks for. A format the clause names is read off the
/// statement's text, since it is the format this tree would come in, too.
pub fn unread(lines: &[&str]) -> Option<String> {
    let root = lines.first().filter(|l| l.starts_with("QUERY id: "))?;
    if root.contains("is_group_by_with_totals: 1") {
        return Some("WITH TOTALS returns its totals beside the rows, and a typed result is its rows alone: write WITH ROLLUP, whose totals are rows, or select the totals in a statement of their own.".to_string());
    }
    // The top query's own settings: `SETTINGS a=1 b=x`, a level under the root.
    let settings = lines.iter().find_map(|l| l.strip_prefix("  SETTINGS "))?;
    let set = |name: &str| settings.split_whitespace().find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='));
    ["output_format_binary_encode_types_in_binary_format", "extremes"]
        .into_iter()
        .find(|name| set(name).is_some_and(|value| value != "0"))
        .map(|name| format!("The statement's SETTINGS turns on {name}, which changes what a result holds beside its rows, and Reylo reads rows alone: take the setting out."))
}

/// The settings that may drop the one row an aggregate returns, which a `SETTINGS` clause names
/// as the session's own are read (`ROW_SETTINGS`).
const DROPS: [&str; 4] = ["empty_result_for_aggregation_by_empty_set", "limit", "offset", "additional_result_filter"];

/// The setting that has each shard of a distributed table answer for itself: a row for each, of an
/// aggregate or within a limit.
pub const UNMERGED: &str = "distributed_group_by_no_merge";

/// What the tree's lines prove.
pub fn rows(lines: &[&str]) -> Rows {
    let Some(root) = lines.first().filter(|l| l.starts_with("QUERY id: ")) else { return Rows::default() };
    // A setting a SETTINGS clause names, the statement's own or a subquery's. A string a clause
    // sets may hold a line break, which parts the clause's line, so each line's words are read.
    let set = |names: &[&str]| lines.iter().flat_map(|l| l.split_whitespace()).any(|pair| names.iter().any(|name| pair.strip_prefix(*name).is_some_and(|rest| rest.starts_with('='))));
    if set(&[UNMERGED]) {
        return Rows::default();
    }
    let sections = sections(lines);
    let body = |name: &str| sections.iter().find(|(heading, _)| *heading == name).map(|(_, body)| body.as_slice());
    let has = |name: &str| sections.iter().any(|(heading, _)| *heading == name);
    let ties = root.contains("is_limit_with_ties: 1");
    // The LIMIT's first line is its value; under it, the expression it was folded from, where it
    // was: a value with nothing under it is a number the statement writes.
    let limit = match body("LIMIT") {
        None => Some(None),
        Some(body) => body.first().filter(|_| body.len() == 1).and_then(|line| constant(line)).map(Some),
    };
    let settings = set(&DROPS);
    let grouped = root.contains("is_group_by") || root.contains("group_by_type") || has("GROUP BY");
    let dropped = ["HAVING", "QUALIFY", "OFFSET", "LIMIT BY", "LIMIT BY LIMIT"].iter().any(|s| has(s));
    let filled = lines.iter().any(|l| l.contains("with_fill: 1"));
    let projection = body("PROJECTION").map(own).unwrap_or_default();
    // A function's own line: a constant's text may hold the same words.
    let aggregated = projection.iter().any(|l| l.trim_start().starts_with("FUNCTION ") && l.contains("function_type: aggregate"));
    // An arrayJoin in what the statement computes after it aggregates; one in what it reads, its
    // tables and its WHERE, multiplies the rows the aggregate takes in.
    let multiplied = sections.iter().filter(|(heading, _)| !matches!(*heading, "JOIN TREE" | "WHERE" | "PREWHERE")).any(|(_, body)| own(body).iter().any(|l| l.contains("function_name: arrayJoin,")));
    let aggregate = aggregated && !multiplied && !grouped && !filled;
    let at_most_one = aggregate || (!ties && matches!(limit, Some(Some(0 | 1))));
    let kept = matches!(limit, Some(None)) || matches!(limit, Some(Some(n)) if n >= 1);
    let one = aggregate && !dropped && !settings && !ties && kept;
    Rows { at_most_one, one, aggregate }
}

/// The server's tree of a SELECT, a node a line: how far in it stands, and its text.
struct Tree<'a> {
    nodes: Vec<(usize, &'a str)>,
    /// Where the node of each id is first written: one that stands in several places is written whole at each.
    ids: HashMap<&'a str, usize>,
    /// The lines that are a query's column names, and no nodes.
    names: Vec<bool>,
    /// Whether a type is one a parameter written as a constant has.
    constant: &'a dyn Fn(&str) -> bool,
}

/// A `COLUMN` node's name, its type, and the id of the node its values come from.
fn column(text: &str) -> Option<(&str, &str, &str)> {
    let rest = text.strip_prefix("COLUMN id: ")?;
    let (rest, source) = rest.rsplit_once(", source_id: ")?;
    let (rest, ty) = rest.rsplit_once(", result_type: ")?;
    Some((rest.split_once("column_name: ")?.1, ty, source))
}

impl<'a> Tree<'a> {
    /// The tree the lines are, where they are one. A name or a text with a line break in it
    /// parts its node's line, and what stands under the rest of the line would be read as
    /// under nothing, or as a node the query does not have. So every line is a level under one
    /// before it, each list has the nodes it counts, and each query its columns once, as many
    /// as it computes.
    fn read(lines: &[&'a str], constant: &'a dyn Fn(&str) -> bool) -> Option<Tree<'a>> {
        let placed = |(i, line): (usize, &&str)| match i {
            0 => indent(line) == 0,
            _ => indent(line) > 0 && indent(line) % 2 == 0 && indent(line) <= indent(lines[i - 1]) + 2,
        };
        if lines.is_empty() || !lines.iter().enumerate().all(placed) {
            return None;
        }
        let tree = Tree::of(lines, constant);
        let counted = |i: usize| match tree.nodes[i].1.rsplit_once("nodes: ").map(|(_, count)| count.parse::<usize>()) {
            Some(Ok(count)) => count == tree.children(i).len(),
            _ => false,
        };
        let once = |i: usize| {
            let sections = |name: &str| tree.children(i).into_iter().filter(|&child| tree.nodes[child].1 == name).count();
            sections("PROJECTION") <= 1 && sections("PROJECTION COLUMNS") <= 1 && tree.columns(i).is_none_or(|columns| tree.listed(i, "PROJECTION").is_some_and(|items| items.len() == columns.len()))
        };
        let sound = (0..tree.nodes.len()).filter(|&i| !tree.names[i]).all(|i| match tree.kind(i) {
            "LIST" => counted(i),
            "QUERY" => once(i),
            _ => true,
        });
        sound.then_some(tree)
    }

    fn of(lines: &[&'a str], constant: &'a dyn Fn(&str) -> bool) -> Tree<'a> {
        let nodes: Vec<(usize, &str)> = lines.iter().map(|line| (indent(line), line.trim_start_matches(' '))).collect();
        let mut names = vec![false; nodes.len()];
        let mut ids = HashMap::new();
        let mut columns: Option<usize> = None;
        for (i, &(depth, text)) in nodes.iter().enumerate() {
            if columns.is_some_and(|under| depth > under) {
                names[i] = true;
                continue;
            }
            columns = (text == "PROJECTION COLUMNS").then_some(depth);
            let kind = text.split(' ').next().unwrap_or_default();
            if let Some(id) = text[kind.len()..].strip_prefix(" id: ") {
                ids.entry(id.split(',').next().unwrap_or_default()).or_insert(i);
            }
        }
        Tree { nodes, ids, names, constant }
    }

    fn kind(&self, i: usize) -> &'a str {
        self.nodes[i].1.split(' ').next().unwrap_or_default()
    }

    /// Past the last line under the node at `i`.
    fn end(&self, i: usize) -> usize {
        (i + 1..self.nodes.len()).find(|&j| self.nodes[j].0 <= self.nodes[i].0).unwrap_or(self.nodes.len())
    }

    /// The lines a level under the node at `i`.
    fn children(&self, i: usize) -> Vec<usize> {
        (i + 1..self.end(i)).filter(|&j| self.nodes[j].0 == self.nodes[i].0 + 2).collect()
    }

    fn section(&self, i: usize, name: &str) -> Option<usize> {
        self.children(i).into_iter().find(|&child| self.nodes[child].1 == name)
    }

    /// The nodes of the list that a section of the node at `i` holds.
    fn listed(&self, i: usize, name: &str) -> Option<Vec<usize>> {
        let list = self.children(self.section(i, name)?).into_iter().find(|&child| self.kind(child) == "LIST")?;
        Some(self.children(list))
    }

    /// The results of the choice between values the node at `i` is, where it is one: an `if`'s
    /// two, a `multiIf`'s after each condition and its last, and a `CASE`'s after each value it
    /// compares with and its last.
    fn results(&self, i: usize) -> Option<Vec<usize>> {
        let name = self.nodes[i].1.strip_prefix("FUNCTION id: ")?.split_once("function_name: ")?.1.split(',').next()?;
        let arguments = self.listed(i, "ARGUMENTS")?;
        let count = arguments.len();
        let places: Vec<usize> = match name {
            "if" if count == 3 => vec![1, 2],
            "multiIf" if count >= 3 && count % 2 == 1 => (1..count).step_by(2).chain([count - 1]).collect(),
            "caseWithExpression" if count >= 4 && count % 2 == 0 => (2..count - 1).step_by(2).chain([count - 1]).collect(),
            _ => return None,
        };
        Some(places.into_iter().map(|place| arguments[place]).collect())
    }

    /// The values the node at `i` may be, added to `picks`: each result of a choice, a text or a
    /// NULL the statement writes, or any value of the type its line says. `None` where its line
    /// says no type.
    fn picks(&self, i: usize, picks: &mut Vec<Pick>) -> Option<()> {
        if let Some(results) = self.results(i) {
            return results.into_iter().try_for_each(|result| self.picks(result, picks));
        }
        let text = self.nodes[i].1;
        // A constant with nothing under it is one the statement writes, and no computed one.
        let written = self.end(i) == i + 1;
        picks.push(if let Some(constant) = text.strip_prefix("CONSTANT id: ") {
            let (value, ty) = constant.split_once("constant_value: ")?.1.rsplit_once(", constant_value_type: ")?;
            match value.strip_prefix('\'').and_then(|value| value.strip_suffix('\'')) {
                Some(value) if written && ty == "String" && !value.contains(['\\', '\'']) => Pick::Text(value.to_string()),
                _ if written && value == "NULL" && ty == "Nullable(Nothing)" => Pick::Null,
                _ => Pick::Typed(ty.to_string()),
            }
        } else if let Some((_, ty, _)) = column(text) {
            Pick::Typed(ty.to_string())
        } else {
            Pick::Typed(text.strip_prefix("FUNCTION id: ")?.rsplit_once("result_type: ")?.1.to_string())
        });
        Some(())
    }

    /// What the node at `i` is, where it is what a parameter is written as: the function that
    /// makes a column of a constant, or a constant's conversion to a type a parameter written as
    /// one has. With it, the type it has, where its line says one.
    fn stand_in(&self, i: usize) -> Option<Option<&'a str>> {
        let rest = self.nodes[i].1.strip_prefix("FUNCTION id: ")?;
        let Some((_, name)) = rest.split_once("function_name: ") else { return Some(None) };
        let ty = rest.rsplit_once("result_type: ").map(|(_, ty)| ty);
        match name.split(',').next().unwrap_or_default() {
            "materialize" => Some(ty),
            "CAST" | "_CAST" | "defaultValueOfTypeName" if ty.is_none_or(|ty| (self.constant)(ty)) => Some(ty),
            _ => None,
        }
    }

    /// What the lines from `from` to `to` compute depends on, added to `flow`: each parameter
    /// among them, and those the values of each column among them depend on.
    fn collect(&self, from: usize, to: usize, seen: &mut HashSet<(usize, usize)>, flow: &mut Flow) {
        for i in (from..to).filter(|&i| !self.names[i]) {
            match self.stand_in(i) {
                Some(Some(ty)) => flow.of.push(ty.to_string()),
                Some(None) => flow.unknown = true,
                None => {}
            }
            if self.kind(i) == "COLUMN" {
                match column(self.nodes[i].1) {
                    Some((name, ty, source)) => self.sourced(source, name, ty, seen, flow),
                    None => flow.unknown = true,
                }
            }
        }
    }

    /// What the column `name` of the node with the id `source` depends on, added to `flow`. A
    /// source the engine does not know may be anything, so its columns may depend on a parameter.
    fn sourced(&self, source: &str, name: &str, ty: &str, seen: &mut HashSet<(usize, usize)>, flow: &mut Flow) {
        let Some(&at) = self.ids.get(source) else {
            flow.unknown = true;
            return;
        };
        match self.kind(at) {
            // A join's column holds the columns it joins under it, a lambda's argument, whose source
            // is the lambda or its list of arguments, is of what its function is given, and an
            // INTERPOLATE's column is the query's own: each is read where it stands.
            "JOIN" | "LAMBDA" | "ARGUMENTS" | "INTERPOLATE" => {}
            // A table's rows are its own, and a table function's are of its arguments, in a way
            // no type of theirs says.
            "TABLE" | "TABLE_FUNCTION" => {
                let mut given = Flow::default();
                self.collect(at + 1, self.end(at), seen, &mut given);
                flow.unknown |= given.depends();
            }
            // The expression the ARRAY JOIN unfolds into the column.
            "ARRAY_JOIN" => {
                let named: Vec<usize> = self.listed(at, "JOIN EXPRESSIONS").unwrap_or_default().into_iter().filter(|&c| column(self.nodes[c].1).is_some_and(|(joined, ..)| joined == name)).collect();
                flow.unknown |= named.is_empty();
                for c in named {
                    if seen.insert((at, c)) {
                        self.collect(c + 1, self.end(c), seen, flow);
                    }
                }
            }
            "QUERY" | "UNION" => {
                let columns = self.columns(at).unwrap_or_default();
                // The column by its name and its type, or by its name where a union's type is another.
                let own = format!("{name} {ty}");
                let mut places: Vec<usize> = (0..columns.len()).filter(|&c| columns[c] == own).collect();
                if places.is_empty() {
                    places = (0..columns.len()).filter(|&c| columns[c].strip_prefix(name).is_some_and(|rest| rest.starts_with(' '))).collect();
                }
                flow.unknown |= places.is_empty();
                for c in places {
                    self.produced(at, c, seen, flow);
                }
            }
            _ => flow.unknown = true,
        }
    }

    /// The columns of the query at `i`, each a name and a type, or of a set operation's first query.
    fn columns(&self, i: usize) -> Option<Vec<&'a str>> {
        match self.kind(i) {
            "QUERY" => Some(self.children(self.section(i, "PROJECTION COLUMNS")?).into_iter().map(|c| self.nodes[c].1).collect()),
            "UNION" => self.columns(*self.listed(i, "QUERIES")?.first()?),
            _ => None,
        }
    }

    /// Whether the column numbered `c` of the query at `i` is a conversion to a type the statement
    /// names: a `CAST` at the top of what it computes, or of what the server folded it from,
    /// whose type is a constant the statement writes, with nothing under it that it was computed
    /// from. A column read from a subquery is what the subquery's column is, and a set
    /// operation's what each of its queries' is.
    fn converted(&self, i: usize, c: usize, seen: &mut HashSet<(usize, usize)>) -> bool {
        match self.kind(i) {
            "QUERY" => {
                let Some(item) = self.listed(i, "PROJECTION").and_then(|items| items.get(c).copied()) else { return false };
                // What a constant was folded from stands under it.
                let top = match self.kind(item) {
                    "CONSTANT" => self.section(item, "EXPRESSION").and_then(|folded| self.children(folded).first().copied()).unwrap_or(item),
                    _ => item,
                };
                let function = |name: &str| self.nodes[top].1.strip_prefix("FUNCTION id: ").and_then(|rest| rest.split_once("function_name: ")).is_some_and(|(_, after)| after.split(',').next() == Some(name));
                if function("CAST") || function("_CAST") {
                    // The type it converts to is its last argument.
                    let named = self.listed(top, "ARGUMENTS").and_then(|arguments| arguments.last().copied());
                    return named.is_some_and(|ty| self.kind(ty) == "CONSTANT" && self.end(ty) == ty + 1);
                }
                // A subquery's column, by its name and its type, each one it may be.
                let Some((name, ty, source)) = column(self.nodes[top].1) else { return false };
                let Some(&at) = self.ids.get(source) else { return false };
                let Some(columns) = self.columns(at) else { return false };
                let own = format!("{name} {ty}");
                let places: Vec<usize> = (0..columns.len()).filter(|&place| columns[place] == own).collect();
                seen.insert((i, c)) && !places.is_empty() && places.into_iter().all(|place| self.converted(at, place, seen))
            }
            "UNION" => self.listed(i, "QUERIES").is_some_and(|queries| !queries.is_empty() && queries.into_iter().all(|query| self.converted(query, c, seen))),
            _ => false,
        }
    }

    /// What the column numbered `c` of the query at `i` depends on, added to `flow`: in a set
    /// operation, the column of each of its queries.
    fn produced(&self, i: usize, c: usize, seen: &mut HashSet<(usize, usize)>, flow: &mut Flow) {
        match self.kind(i) {
            // A column read a second time has been added.
            "QUERY" if !seen.insert((i, c)) => {}
            "QUERY" => match self.listed(i, "PROJECTION").and_then(|items| items.get(c).copied()) {
                Some(item) => self.collect(item, self.end(item), seen, flow),
                None => flow.unknown = true,
            },
            "UNION" => match self.listed(i, "QUERIES") {
                Some(queries) => queries.into_iter().for_each(|query| self.produced(query, c, seen, flow)),
                None => flow.unknown = true,
            },
            _ => flow.unknown = true,
        }
    }
}

/// Which of a SELECT's columns hold values that depend on a parameter, from the server's tree of
/// the SELECT with each parameter written as what stands for it: a column is `materialize(…)`,
/// and a constant a `CAST` to its type, which `constant` tells from the statement's own. The
/// tree has each alias and each `WITH` written out where it is used, each `*` as its columns, and
/// each column of a subquery by the query it comes from, so a column depends on a parameter where
/// its expression holds one, or reads a column that does: of a subquery, a set operation, an
/// `ARRAY JOIN` or a table function. A row a `WITH FILL` adds is of the parameters that bound it.
/// A parameter that only keeps rows, in a `WHERE` or a join's condition, is in no column.
/// `None` where the lines are no query's tree, and where an `INTERPOLATE` computes with a
/// parameter: it converts what it computes to its column's type, inside the SELECT.
pub fn flows(lines: &[&str], constant: &dyn Fn(&str) -> bool) -> Option<Vec<Flow>> {
    let tree = Tree::read(lines, constant)?;
    let count = tree.columns(0)?.len();
    let within = |from: usize, to: usize| {
        let mut flow = Flow::default();
        tree.collect(from, to, &mut HashSet::new(), &mut flow);
        flow
    };
    if (0..tree.nodes.len()).any(|i| !tree.names[i] && tree.nodes[i].1 == "INTERPOLATE" && within(i + 1, tree.end(i)).depends()) {
        return None;
    }
    let mut flows: Vec<Flow> = (0..count)
        .map(|c| {
            let mut flow = Flow::default();
            tree.produced(0, c, &mut HashSet::new(), &mut flow);
            flow
        })
        .collect();
    // A row a WITH FILL adds is of the parameters that bound it, whatever its columns compute.
    if lines.iter().any(|line| line.contains("with_fill: 1")) && (0..tree.nodes.len()).any(|i| !tree.names[i] && tree.stand_in(i).is_some()) {
        flows.iter_mut().for_each(|flow| flow.unknown = true);
    }
    Some(flows)
}

/// The values each of a SELECT's columns is one of, where the column is a choice between values:
/// an `if`, a `multiIf` or a `CASE`, whose results may be such choices themselves. `None` of any
/// other column, and of every column where the lines are no one query's tree.
pub fn choices(lines: &[&str]) -> Vec<Option<Vec<Pick>>> {
    let Some(tree) = Tree::read(lines, &|_| false).filter(|tree| tree.kind(0) == "QUERY") else { return Vec::new() };
    let chosen = |item: usize| {
        tree.results(item)?;
        let mut picks = Vec::new();
        tree.picks(item, &mut picks).map(|_| picks)
    };
    tree.listed(0, "PROJECTION").unwrap_or_default().into_iter().map(chosen).collect()
}

/// Which of a SELECT's columns are each a conversion to a type the statement names, a `CAST` at
/// the top of what the column computes: the type such a column has is the statement's to say,
/// whatever value a parameter the server read as a constant has. `None` where the lines are no
/// query's tree.
pub fn conversions(lines: &[&str]) -> Option<Vec<bool>> {
    let tree = Tree::read(lines, &|_| false)?;
    let count = tree.columns(0)?.len();
    Some((0..count).map(|c| tree.converted(0, c, &mut HashSet::new())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's own trees, of ClickHouse 26.9, each of the statement named above it.
    const FLOWS: &[(&str, &str, &[&str], &[bool])] = &[
        // a subquery's column, read through a star: SELECT * FROM (SELECT 11 AS id, materialize(defaultValueOfTypeName('UInt64')) AS a)
        ("a subquery's column, read through a star", r#"QUERY id: 0
  PROJECTION COLUMNS
    id UInt8
    a UInt64
  PROJECTION
    LIST id: 1, nodes: 2
      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
      COLUMN id: 4, column_name: a, result_type: UInt64, source_id: 3
  JOIN TREE
    QUERY id: 3, alias: __table1, is_subquery: 1
      PROJECTION COLUMNS
        id UInt8
        a UInt64
      PROJECTION
        LIST id: 5, nodes: 2
          CONSTANT id: 6, constant_value: UInt64_11, constant_value_type: UInt8
          FUNCTION id: 7, function_name: materialize, function_type: ordinary, result_type: UInt64
            ARGUMENTS
              LIST id: 8, nodes: 1
                CONSTANT id: 9, constant_value: UInt64_0, constant_value_type: UInt64
                  EXPRESSION
                    FUNCTION id: 10, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                      ARGUMENTS
                        LIST id: 11, nodes: 1
                          CONSTANT id: 12, constant_value: 'UInt64', constant_value_type: String
      JOIN TREE
        TABLE id: 13, alias: __table2, table_name: system.one"#, &[], &[false, true]),
        // a WITH's name, and a name of what computes with it: WITH materialize(defaultValueOfTypeName('UInt64')) AS x SELECT 10, x, x + 1 AS y
        ("a WITH's name, and a name of what computes with it", r#"QUERY id: 0
  PROJECTION COLUMNS
    10 UInt8
    x UInt64
    y UInt64
  PROJECTION
    LIST id: 1, nodes: 3
      CONSTANT id: 2, constant_value: UInt64_10, constant_value_type: UInt8
      FUNCTION id: 3, function_name: materialize, function_type: ordinary, result_type: UInt64
        ARGUMENTS
          LIST id: 4, nodes: 1
            CONSTANT id: 5, constant_value: UInt64_0, constant_value_type: UInt64
              EXPRESSION
                FUNCTION id: 6, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                  ARGUMENTS
                    LIST id: 7, nodes: 1
                      CONSTANT id: 8, constant_value: 'UInt64', constant_value_type: String
      FUNCTION id: 9, function_name: plus, function_type: ordinary, result_type: UInt64
        ARGUMENTS
          LIST id: 10, nodes: 2
            FUNCTION id: 3, function_name: materialize, function_type: ordinary, result_type: UInt64
              ARGUMENTS
                LIST id: 4, nodes: 1
                  CONSTANT id: 5, constant_value: UInt64_0, constant_value_type: UInt64
                    EXPRESSION
                      FUNCTION id: 6, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                        ARGUMENTS
                          LIST id: 7, nodes: 1
                            CONSTANT id: 8, constant_value: 'UInt64', constant_value_type: String
            CONSTANT id: 11, constant_value: UInt64_1, constant_value_type: UInt8
  JOIN TREE
    TABLE id: 12, alias: __table1, table_name: system.one"#, &[], &[false, true, true]),
        // a set operation's column, of any of its queries: SELECT id FROM src UNION ALL SELECT materialize(defaultValueOfTypeName('UInt64'))
        ("a set operation's column, of any of its queries", r#"UNION id: 0, union_mode: UNION_ALL
  QUERIES
    LIST id: 1, nodes: 2
      QUERY id: 2, alias: __table1
        PROJECTION COLUMNS
          id UInt8
        PROJECTION
          LIST id: 3, nodes: 1
            COLUMN id: 4, column_name: id, result_type: UInt8, source_id: 5
        JOIN TREE
          TABLE id: 5, alias: __table2, table_name: default.src
      QUERY id: 6, alias: __table3
        PROJECTION COLUMNS
          materialize(defaultValueOfTypeName('UInt64')) UInt64
        PROJECTION
          LIST id: 7, nodes: 1
            FUNCTION id: 8, function_name: materialize, function_type: ordinary, result_type: UInt64
              ARGUMENTS
                LIST id: 9, nodes: 1
                  CONSTANT id: 10, constant_value: UInt64_0, constant_value_type: UInt64
                    EXPRESSION
                      FUNCTION id: 11, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                        ARGUMENTS
                          LIST id: 12, nodes: 1
                            CONSTANT id: 13, constant_value: 'UInt64', constant_value_type: String
        JOIN TREE
          TABLE id: 14, alias: __table4, table_name: system.one"#, &[], &[true]),
        // a common table expression's column: WITH c AS (SELECT id, materialize(defaultValueOfTypeName('UInt64')) + 1 AS v FROM src) SELECT c.id, v FROM c
        ("a common table expression's column", r#"QUERY id: 0
  PROJECTION COLUMNS
    id UInt8
    v UInt64
  PROJECTION
    LIST id: 1, nodes: 2
      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
      COLUMN id: 4, column_name: v, result_type: UInt64, source_id: 3
  JOIN TREE
    QUERY id: 3, alias: __table1, is_subquery: 1, is_cte: 1, cte_name: c
      PROJECTION COLUMNS
        id UInt8
        v UInt64
      PROJECTION
        LIST id: 5, nodes: 2
          COLUMN id: 6, column_name: id, result_type: UInt8, source_id: 7
          FUNCTION id: 8, function_name: plus, function_type: ordinary, result_type: UInt64
            ARGUMENTS
              LIST id: 9, nodes: 2
                FUNCTION id: 10, function_name: materialize, function_type: ordinary, result_type: UInt64
                  ARGUMENTS
                    LIST id: 11, nodes: 1
                      CONSTANT id: 12, constant_value: UInt64_0, constant_value_type: UInt64
                        EXPRESSION
                          FUNCTION id: 13, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                            ARGUMENTS
                              LIST id: 14, nodes: 1
                                CONSTANT id: 15, constant_value: 'UInt64', constant_value_type: String
                CONSTANT id: 16, constant_value: UInt64_1, constant_value_type: UInt8
      JOIN TREE
        TABLE id: 7, alias: __table2, table_name: default.src"#, &[], &[false, true]),
        // an ARRAY JOIN's column, and no column of a join or of a condition: SELECT id, e, y FROM src ARRAY JOIN [materialize(defaultValueOfTypeName('UInt64'))] AS e JOIN o USING (id) WHERE id IN (SELECT materialize(defaultValueOfTypeName('UInt64')))
        ("an ARRAY JOIN's column, and no column of a join or of a condition", r#"QUERY id: 0
  PROJECTION COLUMNS
    id UInt8
    e UInt64
    y UInt64
  PROJECTION
    LIST id: 1, nodes: 3
      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
      COLUMN id: 4, column_name: __array_join_exp_1, result_type: UInt64, source_id: 5
      COLUMN id: 6, column_name: y, result_type: UInt64, source_id: 7
  JOIN TREE
    JOIN id: 8, strictness: ALL, kind: INNER
      LEFT TABLE EXPRESSION
        ARRAY_JOIN id: 5, is_left: 0
          TABLE EXPRESSION
            TABLE id: 3, alias: __table2, table_name: default.src
          JOIN EXPRESSIONS
            LIST id: 9, nodes: 1
              COLUMN id: 10, alias: __array_join_exp_1, column_name: __array_join_exp_1, result_type: UInt64, source_id: 5
                EXPRESSION
                  FUNCTION id: 11, function_name: array, function_type: ordinary, result_type: Array(UInt64)
                    ARGUMENTS
                      LIST id: 12, nodes: 1
                        FUNCTION id: 13, function_name: materialize, function_type: ordinary, result_type: UInt64
                          ARGUMENTS
                            LIST id: 14, nodes: 1
                              CONSTANT id: 15, constant_value: UInt64_0, constant_value_type: UInt64
                                EXPRESSION
                                  FUNCTION id: 16, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                                    ARGUMENTS
                                      LIST id: 17, nodes: 1
                                        CONSTANT id: 18, constant_value: 'UInt64', constant_value_type: String
      RIGHT TABLE EXPRESSION
        TABLE id: 7, alias: __table3, table_name: default.o
      JOIN EXPRESSION
        LIST id: 19, nodes: 1
          COLUMN id: 20, column_name: id, result_type: UInt8, source_id: 8
            EXPRESSION
              LIST id: 21, nodes: 2
                COLUMN id: 22, column_name: id, result_type: UInt8, source_id: 3
                COLUMN id: 23, column_name: id, result_type: UInt8, source_id: 7
  WHERE
    FUNCTION id: 24, function_name: in, function_type: ordinary, result_type: UInt8
      ARGUMENTS
        LIST id: 25, nodes: 2
          COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
          QUERY id: 26, is_subquery: 1
            PROJECTION COLUMNS
              materialize(defaultValueOfTypeName('UInt64')) UInt64
            PROJECTION
              LIST id: 27, nodes: 1
                FUNCTION id: 28, function_name: materialize, function_type: ordinary, result_type: UInt64
                  ARGUMENTS
                    LIST id: 29, nodes: 1
                      CONSTANT id: 30, constant_value: UInt64_0, constant_value_type: UInt64
                        EXPRESSION
                          FUNCTION id: 31, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                            ARGUMENTS
                              LIST id: 32, nodes: 1
                                CONSTANT id: 33, constant_value: 'UInt64', constant_value_type: String
            JOIN TREE
              TABLE id: 34, alias: __table1, table_name: system.one"#, &[], &[false, true, false]),
        // a parameter that only keeps rows is in no column: SELECT id, x FROM src WHERE x = materialize(defaultValueOfTypeName('UInt64')) ORDER BY id LIMIT 3
        ("a parameter that only keeps rows is in no column", r#"QUERY id: 0
  PROJECTION COLUMNS
    id UInt8
    x UInt64
  PROJECTION
    LIST id: 1, nodes: 2
      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
      COLUMN id: 4, column_name: x, result_type: UInt64, source_id: 3
  JOIN TREE
    TABLE id: 3, alias: __table1, table_name: default.src
  WHERE
    FUNCTION id: 5, function_name: equals, function_type: ordinary, result_type: UInt8
      ARGUMENTS
        LIST id: 6, nodes: 2
          COLUMN id: 4, column_name: x, result_type: UInt64, source_id: 3
          FUNCTION id: 7, function_name: materialize, function_type: ordinary, result_type: UInt64
            ARGUMENTS
              LIST id: 8, nodes: 1
                CONSTANT id: 9, constant_value: UInt64_0, constant_value_type: UInt64
                  EXPRESSION
                    FUNCTION id: 10, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                      ARGUMENTS
                        LIST id: 11, nodes: 1
                          CONSTANT id: 12, constant_value: 'UInt64', constant_value_type: String
  ORDER BY
    LIST id: 13, nodes: 1
      SORT id: 14, sort_direction: ASCENDING, with_fill: 0
        EXPRESSION id
          COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
  LIMIT
    CONSTANT id: 15, constant_value: UInt64_3, constant_value_type: UInt64"#, &[], &[false, false]),
        // a scalar subquery that holds one: SELECT 1, (SELECT max(x) FROM src WHERE x = materialize(defaultValueOfTypeName('UInt64')))
        ("a scalar subquery that holds one", r#"QUERY id: 0
  PROJECTION COLUMNS
    1 UInt8
    _subquery_1 Nullable(UInt64)
  PROJECTION
    LIST id: 1, nodes: 2
      CONSTANT id: 2, constant_value: UInt64_1, constant_value_type: UInt8
      CONSTANT id: 3, constant_value: UInt64_0, constant_value_type: Nullable(UInt64)
        EXPRESSION
          QUERY id: 4, is_subquery: 1
            PROJECTION COLUMNS
              max(x) UInt64
            PROJECTION
              LIST id: 5, nodes: 1
                FUNCTION id: 6, function_name: max, function_type: aggregate, result_type: UInt64
                  ARGUMENTS
                    LIST id: 7, nodes: 1
                      COLUMN id: 8, column_name: x, result_type: UInt64, source_id: 9
            JOIN TREE
              TABLE id: 9, table_name: default.src
            WHERE
              FUNCTION id: 10, function_name: equals, function_type: ordinary, result_type: UInt8
                ARGUMENTS
                  LIST id: 11, nodes: 2
                    COLUMN id: 8, column_name: x, result_type: UInt64, source_id: 9
                    FUNCTION id: 12, function_name: materialize, function_type: ordinary, result_type: UInt64
                      ARGUMENTS
                        LIST id: 13, nodes: 1
                          CONSTANT id: 14, constant_value: UInt64_0, constant_value_type: UInt64
                            EXPRESSION
                              FUNCTION id: 15, function_name: defaultValueOfTypeName, function_type: ordinary, result_type: UInt64
                                ARGUMENTS
                                  LIST id: 16, nodes: 1
                                    CONSTANT id: 17, constant_value: 'UInt64', constant_value_type: String
  JOIN TREE
    TABLE id: 18, alias: __table1, table_name: system.one"#, &[], &[false, true]),
        // a constant of a parameter's type, and not the statement's own conversion: SELECT x IN (CAST(1, 'UInt64'), 2), x, CAST(x, 'UInt8'), position('abc' IN CAST('a', 'String')) FROM src WHERE id = CAST(1, 'UInt8') LIMIT CAST(1, 'UInt64')
        ("a constant of a parameter's type, and not the statement's own conversion", r#"QUERY id: 0
  PROJECTION COLUMNS
    in(x, (CAST(1, 'UInt64'), 2)) UInt8
    x UInt64
    CAST(x, 'UInt8') UInt8
    position(CAST('a', 'String'), 'abc') UInt64
  PROJECTION
    LIST id: 1, nodes: 4
      FUNCTION id: 2, function_name: in, function_type: ordinary, result_type: UInt8
        ARGUMENTS
          LIST id: 3, nodes: 2
            COLUMN id: 4, column_name: x, result_type: UInt64, source_id: 5
            CONSTANT id: 6, constant_value: Tuple_(UInt64_1, UInt64_2), constant_value_type: Tuple(UInt64, UInt8)
              EXPRESSION
                FUNCTION id: 7, function_name: tuple, function_type: ordinary, result_type: Tuple(UInt64, UInt8)
                  ARGUMENTS
                    LIST id: 8, nodes: 2
                      CONSTANT id: 9, constant_value: UInt64_1, constant_value_type: UInt64
                        EXPRESSION
                          FUNCTION id: 10, function_name: CAST, function_type: ordinary, result_type: UInt64
                            ARGUMENTS
                              LIST id: 11, nodes: 2
                                CONSTANT id: 12, constant_value: UInt64_1, constant_value_type: UInt8
                                CONSTANT id: 13, constant_value: 'UInt64', constant_value_type: String
                      CONSTANT id: 14, constant_value: UInt64_2, constant_value_type: UInt8
      COLUMN id: 4, column_name: x, result_type: UInt64, source_id: 5
      FUNCTION id: 15, function_name: CAST, function_type: ordinary, result_type: UInt8
        ARGUMENTS
          LIST id: 16, nodes: 2
            COLUMN id: 4, column_name: x, result_type: UInt64, source_id: 5
            CONSTANT id: 17, constant_value: 'UInt8', constant_value_type: String
      CONSTANT id: 18, constant_value: UInt64_0, constant_value_type: UInt64
        EXPRESSION
          FUNCTION id: 19, function_name: position, function_type: ordinary, result_type: UInt64
            ARGUMENTS
              LIST id: 20, nodes: 2
                CONSTANT id: 21, constant_value: 'a', constant_value_type: String
                  EXPRESSION
                    FUNCTION id: 22, function_name: CAST, function_type: ordinary, result_type: String
                      ARGUMENTS
                        LIST id: 23, nodes: 2
                          CONSTANT id: 24, constant_value: 'a', constant_value_type: String
                          CONSTANT id: 25, constant_value: 'String', constant_value_type: String
                CONSTANT id: 26, constant_value: 'abc', constant_value_type: String
  JOIN TREE
    TABLE id: 5, alias: __table1, table_name: default.src
  WHERE
    FUNCTION id: 27, function_name: equals, function_type: ordinary, result_type: UInt8
      ARGUMENTS
        LIST id: 28, nodes: 2
          COLUMN id: 29, column_name: id, result_type: UInt8, source_id: 5
          CONSTANT id: 30, constant_value: UInt64_1, constant_value_type: UInt8
            EXPRESSION
              FUNCTION id: 31, function_name: CAST, function_type: ordinary, result_type: UInt8
                ARGUMENTS
                  LIST id: 32, nodes: 2
                    CONSTANT id: 33, constant_value: UInt64_1, constant_value_type: UInt8
                    CONSTANT id: 34, constant_value: 'UInt8', constant_value_type: String
  LIMIT
    CONSTANT id: 35, constant_value: UInt64_1, constant_value_type: UInt64
      EXPRESSION
        FUNCTION id: 36, function_name: CAST, function_type: ordinary, result_type: UInt64
          ARGUMENTS
            LIST id: 37, nodes: 2
              CONSTANT id: 38, constant_value: UInt64_1, constant_value_type: UInt8
              CONSTANT id: 39, constant_value: 'UInt64', constant_value_type: String"#, &["UInt64", "String"], &[true, false, false, true]),
        // a table function's rows are of its arguments: SELECT number, 1 FROM numbers(CAST(3, 'UInt64'))
        ("a table function's rows are of its arguments", r#"QUERY id: 0
  PROJECTION COLUMNS
    number UInt64
    1 UInt8
  PROJECTION
    LIST id: 1, nodes: 2
      COLUMN id: 2, column_name: number, result_type: UInt64, source_id: 3
      CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt8
  JOIN TREE
    TABLE_FUNCTION id: 3, alias: __table1, table_function_name: numbers
      ARGUMENTS
        LIST id: 5, nodes: 1
          CONSTANT id: 6, constant_value: UInt64_3, constant_value_type: UInt64
            EXPRESSION
              FUNCTION id: 7, function_name: CAST, function_type: ordinary, result_type: UInt64
                ARGUMENTS
                  LIST id: 8, nodes: 2
                    CONSTANT id: 9, constant_value: UInt64_3, constant_value_type: UInt8
                    CONSTANT id: 10, constant_value: 'UInt64', constant_value_type: String"#, &["UInt64"], &[true, false]),
        // a row a WITH FILL adds is of what bounds it: SELECT id, 1 FROM src ORDER BY id WITH FILL TO CAST(9, 'UInt8')
        ("a row a WITH FILL adds is of what bounds it", r#"QUERY id: 0
  PROJECTION COLUMNS
    id UInt8
    1 UInt8
  PROJECTION
    LIST id: 1, nodes: 2
      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
      CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt8
  JOIN TREE
    TABLE id: 3, alias: __table1, table_name: default.src
  ORDER BY
    LIST id: 5, nodes: 1
      SORT id: 6, sort_direction: ASCENDING, with_fill: 1
        EXPRESSION id
          COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3
        FILL TO
          CONSTANT id: 7, constant_value: UInt64_9, constant_value_type: UInt8
            EXPRESSION
              FUNCTION id: 8, function_name: CAST, function_type: ordinary, result_type: UInt8
                ARGUMENTS
                  LIST id: 9, nodes: 2
                    CONSTANT id: 10, constant_value: UInt64_9, constant_value_type: UInt8
                    CONSTANT id: 11, constant_value: 'UInt8', constant_value_type: String"#, &["UInt8"], &[true, true]),
    ];

    #[test]
    fn a_column_is_a_conversion_where_a_cast_stands_at_its_top() {
        let read = |tree: &str| conversions(&tree.lines().collect::<Vec<_>>());
        let query = |items: &str, count: usize| format!("QUERY id: 0\n  PROJECTION COLUMNS{}\n  PROJECTION\n    LIST id: 1, nodes: {count}\n{items}\n  JOIN TREE\n    TABLE id: 3, table_name: app.t", (0..count).map(|i| format!("\n    c{i} UInt8")).collect::<String>());
        // A CAST, one the server folded a constant from, and what is neither: a function that computes, and a table's column.
        let to = |depth: usize, id: usize| format!("\n{0}ARGUMENTS\n{0}  LIST id: {id}, nodes: 2\n{0}    COLUMN id: {1}, column_name: c4, result_type: UInt64, source_id: 3\n{0}    CONSTANT id: {2}, constant_value: 'UInt8', constant_value_type: String", " ".repeat(depth), id + 1, id + 2);
        let items = format!(
            "      FUNCTION id: 4, function_name: _CAST, function_type: ordinary, result_type: UInt8{}\n      CONSTANT id: 5, constant_value: UInt64_1, constant_value_type: UInt8\n        EXPRESSION\n          FUNCTION id: 6, function_name: CAST, function_type: ordinary, result_type: UInt8{}\n      FUNCTION id: 7, function_name: tupleElement, function_type: ordinary, result_type: UInt8\n      CONSTANT id: 8, constant_value: UInt64_1, constant_value_type: UInt8\n        EXPRESSION\n          FUNCTION id: 9, function_name: tupleElement, function_type: ordinary, result_type: UInt8\n      COLUMN id: 10, column_name: c4, result_type: UInt8, source_id: 3",
            to(8, 20),
            to(12, 30)
        );
        let items = items.as_str();
        assert_eq!(read(&query(items, 5)), Some(vec![true, true, false, false, false]));
        // A CAST to a type the statement computes names no one type: the constant it converts to was folded from something.
        let computed = query(items, 5).replacen("constant_value: 'UInt8', constant_value_type: String", "constant_value: 'UInt8', constant_value_type: String\n              EXPRESSION\n                FUNCTION id: 40, function_name: if, function_type: ordinary, result_type: String", 1);
        assert_eq!(read(&computed), Some(vec![false, true, false, false, false]));
        // A subquery's column is what the subquery's is, and a set operation's what each of its queries' is.
        let (_, starred, ..) = FLOWS[0];
        assert_eq!(read(starred), Some(vec![false, false]));
        let cast = |depth: usize| format!("{0}ARGUMENTS\n{0}  LIST id: 50, nodes: 2\n{0}    CONSTANT id: 51, constant_value: UInt64_1, constant_value_type: UInt8\n{0}    CONSTANT id: 52, constant_value: 'UInt8', constant_value_type: String", " ".repeat(depth));
        let through = format!("QUERY id: 0\n  PROJECTION COLUMNS\n    v UInt8\n  PROJECTION\n    LIST id: 1, nodes: 1\n      COLUMN id: 2, column_name: v, result_type: UInt8, source_id: 3\n  JOIN TREE\n    QUERY id: 3, is_subquery: 1\n      PROJECTION COLUMNS\n        v UInt8\n      PROJECTION\n        LIST id: 4, nodes: 1\n          FUNCTION id: 5, function_name: CAST, function_type: ordinary, result_type: UInt8\n{}", cast(12));
        let through = through.as_str();
        assert_eq!(read(through), Some(vec![true]));
        let union = format!("UNION id: 0, union_mode: UNION_ALL\n  QUERIES\n    LIST id: 1, nodes: 2\n      QUERY id: 2\n        PROJECTION COLUMNS\n          v UInt8\n        PROJECTION\n          LIST id: 3, nodes: 1\n            FUNCTION id: 4, function_name: CAST, function_type: ordinary, result_type: UInt8\n{0}\n      QUERY id: 5\n        PROJECTION COLUMNS\n          v UInt8\n        PROJECTION\n          LIST id: 6, nodes: 1\n            FUNCTION id: 7, function_name: plus, function_type: ordinary, result_type: UInt8\n{0}", cast(14));
        assert_eq!(read(&union), Some(vec![false]));
        assert_eq!(read(&union.replace("function_name: plus", "function_name: _CAST")), Some(vec![true]));
        assert_eq!(read("Expression"), None);
    }

    /// Whether each column depends on a parameter.
    fn depends(lines: &[&str], constant: &dyn Fn(&str) -> bool) -> Option<Vec<bool>> {
        flows(lines, constant).map(|flows| flows.iter().map(Flow::depends).collect())
    }

    #[test]
    fn a_column_s_parameters_are_told_by_the_types_of_what_stands_for_them() {
        let read = |tree: &str, constants: &[&str]| flows(&tree.lines().collect::<Vec<_>>(), &|ty| constants.contains(&ty)).unwrap();
        let flow = |of: &[&str], unknown: bool| Flow { of: of.iter().map(|ty| ty.to_string()).collect(), unknown };
        // Each stand-in a column's values come from, by its type, through a subquery and a name.
        let (_, starred, ..) = FLOWS[0];
        assert_eq!(read(starred, &[]), [flow(&[], false), flow(&["UInt64"], false)]);
        let (_, named, ..) = FLOWS[1];
        assert_eq!(read(named, &[])[2], flow(&["UInt64"], false));
        // A table function's rows are of its arguments in a way no type says, and so is a row a WITH FILL adds.
        let (_, numbers, ..) = FLOWS[8];
        assert_eq!(read(numbers, &["UInt64"]), [flow(&[], true), flow(&[], false)]);
        let (_, filled, ..) = FLOWS[9];
        assert!(read(filled, &["UInt8"]).iter().all(|flow| flow.unknown));
    }

    #[test]
    fn a_column_is_a_choice_of_values_where_an_if_or_a_case_gives_it() {
        let query = |item: &str| format!("QUERY id: 0\n  PROJECTION COLUMNS\n    c0 String\n  PROJECTION\n    LIST id: 1, nodes: 1\n{item}\n  JOIN TREE\n    TABLE id: 3, table_name: app.t");
        let read = |item: &str| choices(&query(item).lines().collect::<Vec<_>>());
        // A node of a function's arguments, as far in as `depth` functions are.
        let node = |depth: usize, text: &str| format!("{:1$}{text}", "", 6 + 6 * depth);
        let text = |id: usize, value: &str| node(1, &format!("CONSTANT id: {id}, constant_value: '{value}', constant_value_type: String"));
        let condition = |id: usize| node(1, &format!("FUNCTION id: {id}, function_name: materialize, function_type: ordinary, result_type: Bool"));
        let chosen = |name: &str, arguments: &[String]| format!("      FUNCTION id: 2, function_name: {name}, function_type: ordinary, result_type: String\n        ARGUMENTS\n          LIST id: 4, nodes: {}\n{}", arguments.len(), arguments.join("\n"));
        let texts = |values: &[&str]| Some(values.iter().map(|value| Pick::Text(value.to_string())).collect::<Vec<_>>());
        assert_eq!(read(&chosen("if", &[condition(5), text(6, "paid"), text(7, "new")])), [texts(&["paid", "new"])]);
        assert_eq!(read(&chosen("multiIf", &[condition(5), text(6, "a"), condition(8), text(7, "b"), text(9, "c")])), [texts(&["a", "b", "c"])]);
        // A CASE that compares a value gives the result after each value it compares with, and its last.
        let compared = node(1, "CONSTANT id: 10, constant_value: UInt64_1, constant_value_type: UInt8");
        assert_eq!(read(&chosen("caseWithExpression", &[condition(5), compared.clone(), text(6, "a"), compared.replace("id: 10", "id: 11"), text(7, "b"), text(9, "c")])), [texts(&["a", "b", "c"])]);
        // A result may be a choice itself, a NULL, or any value of a type: a column's, a function's, a constant's that is computed or holds an escape.
        let inner = format!("{}\n{}\n{}\n{}\n{}\n{}", node(1, "FUNCTION id: 20, function_name: if, function_type: ordinary, result_type: Nullable(String)"), node(1, "  ARGUMENTS"), node(1, "    LIST id: 21, nodes: 3"), condition(22).replacen("", "      ", 1), text(23, "late").replacen("", "      ", 1), node(2, "CONSTANT id: 24, constant_value: NULL, constant_value_type: Nullable(Nothing)"));
        let column = node(1, "COLUMN id: 30, column_name: status, result_type: Enum8('new' = 1), source_id: 3");
        assert_eq!(read(&chosen("multiIf", &[condition(5), inner, condition(8), column, text(9, "it\\\\'s")])), [Some(vec![Pick::Text("late".into()), Pick::Null, Pick::Typed("Enum8('new' = 1)".into()), Pick::Typed("String".into())])]);
        let lowered = node(1, "FUNCTION id: 40, function_name: lower, function_type: ordinary, result_type: String");
        assert_eq!(read(&chosen("if", &[condition(5), text(6, "paid"), lowered])), [Some(vec![Pick::Text("paid".into()), Pick::Typed("String".into())])]);
        // Another function is no choice, and neither is what is no query's tree.
        assert_eq!(read(&chosen("concat", &[text(6, "paid"), text(7, "new")])), [None]);
        assert!(choices(&["Expression"]).is_empty());
    }

    #[test]
    fn a_column_depends_on_a_parameter_where_the_tree_says_its_values_come_from_one() {
        for (what, tree, constants, expected) in FLOWS {
            let lines: Vec<&str> = tree.lines().collect();
            assert_eq!(depends(&lines, &|ty| constants.contains(&ty)).as_deref(), Some(*expected), "{what}");
        }
        // What is no query's tree says nothing, and neither does one a line of which stands where no node can.
        assert_eq!(depends(&[], &|_| false), None);
        assert_eq!(depends(&["Expression"], &|_| false), None);
        let (_, tree, ..) = FLOWS[0];
        let parted = tree.replacen("alias: __table1", "alias: a\nb", 1);
        assert_eq!(depends(&parted.lines().collect::<Vec<_>>(), &|_| false), None);
        // A list with a node more than it counts is one a name forged, and so is a query's second list of columns.
        let forged = tree.replacen("      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3", "      COLUMN id: 2, column_name: id, result_type: UInt8, source_id: 3\n      CONSTANT id: 98, constant_value: UInt64_1, constant_value_type: UInt8", 1);
        assert_ne!(forged, tree);
        assert_eq!(depends(&forged.lines().collect::<Vec<_>>(), &|_| false), None);
        let twice = tree.replacen("  PROJECTION\n", "  PROJECTION COLUMNS\n    id UInt8\n  PROJECTION\n", 1);
        assert_eq!(depends(&twice.lines().collect::<Vec<_>>(), &|_| false), None);
        // An INTERPOLATE that computes with a parameter converts it inside the SELECT, which no column's type shows.
        let filled = "QUERY id: 0\n  PROJECTION COLUMNS\n    a UInt8\n  PROJECTION\n    LIST id: 1, nodes: 1\n      COLUMN id: 2, column_name: a, result_type: UInt8, source_id: 3\n  JOIN TREE\n    TABLE id: 3, table_name: app.t\n  INTERPOLATE\n    LIST id: 4, nodes: 1\n      INTERPOLATE id: 5\n        EXPRESSION\n          COLUMN id: 2, column_name: a, result_type: UInt8, source_id: 3\n        INTERPOLATE_EXPRESSION\n          FUNCTION id: 6, function_name: materialize, function_type: ordinary, result_type: UInt64";
        assert_eq!(depends(&filled.lines().collect::<Vec<_>>(), &|_| false), None);
        assert_eq!(depends(&filled.replace("function_name: materialize", "function_name: plus").lines().collect::<Vec<_>>(), &|_| false), Some(vec![false]));
        // A source the tree does not show, or one of a kind the engine does not know, may be a parameter's.
        let unknown = "QUERY id: 0\n  PROJECTION COLUMNS\n    a UInt8\n    b UInt8\n  PROJECTION\n    LIST id: 1, nodes: 2\n      COLUMN id: 2, column_name: a, result_type: UInt8, source_id: 9\n      COLUMN id: 4, column_name: b, result_type: UInt8, source_id: 3\n  JOIN TREE\n    TABLE id: 3, table_name: app.t";
        assert_eq!(depends(&unknown.lines().collect::<Vec<_>>(), &|_| false), Some(vec![true, false]));
        let other = unknown.replace("TABLE id: 3, table_name: app.t", "SOMETHING id: 3");
        assert_eq!(depends(&other.lines().collect::<Vec<_>>(), &|_| false), Some(vec![true, true]));
    }


    #[test]
    fn a_result_that_is_more_than_rows_or_written_otherwise_is_refused() {
        assert_eq!(unread(&["QUERY id: 0", "  PROJECTION COLUMNS", "    a UInt8"]), None);
        assert!(unread(&["QUERY id: 0, is_group_by_with_totals: 1", "  PROJECTION COLUMNS"]).unwrap().starts_with("WITH TOTALS returns its totals beside the rows"));
        assert!(unread(&["QUERY id: 0", "  SETTINGS max_threads=1 extremes=1"]).unwrap().contains("turns on extremes"));
        assert!(unread(&["QUERY id: 0", "  SETTINGS output_format_binary_encode_types_in_binary_format=1"]).unwrap().contains("output_format_binary_encode_types_in_binary_format"));
        // A setting that is off, another setting, and a subquery's own say nothing.
        assert_eq!(unread(&["QUERY id: 0", "  SETTINGS extremes=0 max_threads=1"]), None);
        assert_eq!(unread(&["QUERY id: 0", "  JOIN TREE", "    QUERY id: 3, is_subquery: 1", "      SETTINGS extremes=1"]), None);
    }

    fn read(tree: &str) -> Rows {
        let lines: Vec<&str> = tree.lines().collect();
        rows(&lines)
    }

    const AGGREGATE: &str = "QUERY id: 0\n  PROJECTION COLUMNS\n    n UInt64\n  PROJECTION\n    LIST id: 1, nodes: 1\n      FUNCTION id: 2, function_name: count, function_type: aggregate, result_type: UInt64\n  JOIN TREE\n    TABLE id: 3, alias: __table1, table_name: app.users";

    #[test]
    fn an_aggregate_with_no_group_by_is_one_row() {
        assert_eq!(read(AGGREGATE), Rows { at_most_one: true, one: true, aggregate: true });
        let limited = format!("{AGGREGATE}\n  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_10, constant_value_type: UInt64");
        assert_eq!(read(&limited), Rows { at_most_one: true, one: true, aggregate: true });
        let one = format!("{AGGREGATE}\n  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt64");
        assert_eq!(read(&one), Rows { at_most_one: true, one: true, aggregate: true });
    }

    #[test]
    fn what_may_drop_or_add_a_row_leaves_it_unproven() {
        // What may drop the aggregate's row leaves at most one, and what may add rows no bound.
        for (after, bounded) in [
            ("  GROUP BY\n    LIST id: 4, nodes: 1\n      CONSTANT id: 5, constant_value: Tuple_(), constant_value_type: Tuple()", false),
            ("  HAVING\n    FUNCTION id: 6, function_name: greater, function_type: ordinary, result_type: UInt8", true),
            ("  OFFSET\n    CONSTANT id: 13, constant_value: UInt64_0, constant_value_type: UInt64", true),
            ("  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_0, constant_value_type: UInt64", true),
            ("  SETTINGS empty_result_for_aggregation_by_empty_set=1", true),
            ("  ORDER BY\n    LIST id: 6, nodes: 1\n      SORT id: 7, sort_direction: ASCENDING, with_fill: 1", false),
            ("  ORDER BY\n    LIST id: 6, nodes: 1\n      SORT id: 7, sort_direction: ASCENDING, with_fill: 0\n        FUNCTION id: 8, function_name: arrayJoin, function_type: ordinary, result_type: UInt8", false),
            ("  QUALIFY\n    FUNCTION id: 4, function_name: equals, function_type: ordinary, result_type: UInt8", true),
        ] {
            let found = read(&format!("{AGGREGATE}\n{after}"));
            assert_eq!((found.one, found.at_most_one), (false, bounded), "{after}");
        }
        assert_eq!(read(&AGGREGATE.replace("QUERY id: 0", "QUERY id: 0, is_group_by_with_totals: 1")), Rows::default());
        // A setting that drops rows, named with others, in the statement's clause or a subquery's;
        // any other setting leaves the row, and so does a window the statement names.
        for (after, one) in [
            ("  SETTINGS join_use_nulls=1 offset=1", false),
            ("  SETTINGS max_threads=2 additional_result_filter=n > 5", false),
            ("  JOIN TREE\n    QUERY id: 3, is_subquery: 1\n      SETTINGS limit=1", false),
            ("  SETTINGS join_use_nulls=1 max_threads=2", true),
            ("  SETTINGS limits=1 max_offset=2", true),
            ("  WINDOW\n    LIST id: 4, nodes: 1\n      WINDOW id: 5, alias: w, frame_type: RANGE", true),
            // A string a clause sets may hold a line break, and the settings after it stand on the next line.
            ("  SETTINGS log_comment=a\nb empty_result_for_aggregation_by_empty_set=1", false),
            // An aggregate's limit folded from a subquery's answer may be none.
            ("  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt64\n      EXPRESSION\n        QUERY id: 5, is_subquery: 1", false),
        ] {
            assert_eq!(read(&format!("{AGGREGATE}\n{after}")).one, one, "{after}");
        }
        // Each shard of a distributed table answers for itself where the statement says so: nothing is bounded.
        assert_eq!(read(&format!("{AGGREGATE}\n  SETTINGS distributed_group_by_no_merge=1")), Rows::default());
        // A constant's text is no function's line.
        let quoted = AGGREGATE.replace("      FUNCTION id: 2, function_name: count, function_type: aggregate, result_type: UInt64", "      CONSTANT id: 2, constant_value: \\'x, function_type: aggregate\\', constant_value_type: String");
        assert_eq!(read(&quoted), Rows::default());
        // An arrayJoin in what the aggregate reads multiplies its input, not its row.
        let read_from = format!("{AGGREGATE}\n  WHERE\n    FUNCTION id: 9, function_name: arrayJoin, function_type: ordinary, result_type: UInt8");
        assert!(read(&read_from).one);
        // An arrayJoin in the list makes several rows of the one, and an aggregate inside a
        // subquery is the subquery's.
        let joined = AGGREGATE.replace("      FUNCTION id: 2, function_name: count, function_type: aggregate, result_type: UInt64", "      FUNCTION id: 2, function_name: count, function_type: aggregate, result_type: UInt64\n      FUNCTION id: 7, function_name: arrayJoin, function_type: ordinary, result_type: UInt8");
        assert_eq!(read(&joined), Rows::default());
        let inner = "QUERY id: 0\n  PROJECTION\n    LIST id: 1, nodes: 1\n      CONSTANT id: 2, constant_value: UInt64_5, constant_value_type: Nullable(UInt64)\n        EXPRESSION\n          QUERY id: 3, is_subquery: 1\n            PROJECTION\n              LIST id: 4, nodes: 1\n                FUNCTION id: 5, function_name: count, function_type: aggregate, result_type: UInt64\n  JOIN TREE\n    TABLE id: 10, table_name: app.users";
        assert_eq!(read(inner), Rows::default());
        // A view that aggregates is a table to the statement that reads it.
        assert_eq!(read("QUERY id: 0\n  PROJECTION\n    LIST id: 1, nodes: 1\n      COLUMN id: 2, column_name: n, result_type: UInt64, source_id: 3\n  JOIN TREE\n    TABLE id: 3, alias: __table1, table_name: app.counted"), Rows::default());
    }

    #[test]
    fn a_limit_of_one_is_at_most_one_row() {
        let limited = "QUERY id: 0\n  PROJECTION\n    LIST id: 1, nodes: 1\n      COLUMN id: 2, column_name: id, result_type: UInt64, source_id: 3\n  JOIN TREE\n    TABLE id: 3, table_name: app.users\n  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt64";
        assert_eq!(read(limited), Rows { at_most_one: true, one: false, aggregate: false });
        // A limit folded from an expression is not the statement's to fix: a parameter as the
        // limit is written as a cast when the tree is asked, and a subquery's answer is computed
        // as the tree is built. The tree shows under the value what it was folded from.
        for folded in ["      EXPRESSION\n        FUNCTION id: 5, function_name: CAST, function_type: ordinary, result_type: UInt64", "      EXPRESSION\n        QUERY id: 5, is_subquery: 1"] {
            assert_eq!(read(&format!("{limited}\n{folded}")), Rows::default());
        }
        assert_eq!(read(&limited.replace("QUERY id: 0", "QUERY id: 0, is_limit_with_ties: 1")), Rows::default());
        assert_eq!(read(&limited.replace("UInt64_1,", "UInt64_2,")), Rows::default());
        // A set operation's root is no query, and neither is text the engine does not know.
        assert_eq!(read("UNION id: 0, union_mode: UNION_ALL\n  QUERIES\n    LIST id: 1, nodes: 2"), Rows::default());
        assert_eq!(read(""), Rows::default());
    }
}
