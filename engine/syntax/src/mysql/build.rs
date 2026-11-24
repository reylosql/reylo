//! MySQL's statements as the AST of `ast`, built from the lossless tree of the grammar: the reading
//! the MySQL engine analyzes. Each construct is built as the parser this AST comes from built it,
//! but where MySQL reads the text otherwise (operator precedence, `UPDATE IGNORE`, bit literals).

use super::ast::helpers::attached_token::AttachedToken;
use super::ast::tokens::{Location, Span};
use super::ast::*;
use crate::mysql;
use crate::{NodeId, Tree};
use std::cell::Cell;

type R<T> = Result<T, String>;

/// How many bytes apart the character counts a column starts from are.
const CHECKPOINT: usize = 256;

/// Builds the AST of a tree.
pub(crate) struct B<'a> {
    t: &'a Tree<'a>,
    /// Where each line starts, for the line and column a span counts in.
    lines: Vec<usize>,
    /// How many characters stand before each [`CHECKPOINT`]th byte, so a column counts at most that
    /// many bytes from the one before it.
    chars: Vec<u32>,
    /// Whether a backslash escapes in a string (no `NO_BACKSLASH_ESCAPES`).
    backslash: bool,
    /// How deep the build is in expressions, queries and tables.
    depth: Cell<usize>,
    bounds: super::Bounds,
}

fn no<T>(what: &str) -> R<T> {
    Err(what.to_string())
}

/// Why a statement that parses has no AST: it nests deeper than [`super::Bounds::depth`].
pub(crate) const TOO_DEEP: &str = "the statement nests too deeply";
/// Why a statement that parses has no AST: a chain of its operators is longer than [`super::Bounds::chain`].
pub(crate) const TOO_LONG_A_CHAIN: &str = "the statement chains too many operators";

impl<'a> B<'a> {
    pub(crate) fn new(t: &'a Tree<'a>, backslash: bool, bounds: super::Bounds) -> Self {
        let mut lines = vec![0];
        lines.extend(t.text.match_indices('\n').map(|(i, _)| i + 1));
        let mut chars = vec![0u32];
        for block in t.text.as_bytes().chunks(CHECKPOINT) {
            chars.push(chars.last().unwrap() + block.iter().filter(|&&b| b & 0xC0 != 0x80).count() as u32);
        }
        B { t, lines, chars, backslash, depth: Cell::new(0), bounds }
    }

    /// Whether a node is an operator: one that joins two operands or follows one, or a set
    /// operation's list of more than one query.
    fn operator(&self, id: NodeId) -> bool {
        let c = self.ch(id);
        match self.k(id) {
            "Expression" | "BoolPri" | "PredicateExpr" | "BitExpr" => c.len() > 1,
            "SimpleExpr" => c.len() == 3 && matches!(self.k(c[1]), "\"||\"" | "\"COLLATE\""),
            "SetOprClauseList" => c.len() > 1,
            _ => false,
        }
    }

    /// How many operators the longest chain of them has: the most on any one path down the tree.
    /// A chain (`a or b or …`, `a + b + …`, `… union all …`) is read into a tree as deep as it is
    /// long, which every walk of the engine and the tree's drop recurse through, while operators
    /// side by side, in separate columns or conditions, cost nothing.
    fn chain(&self) -> usize {
        let mut longest = 0;
        let mut stack = vec![(self.t.root, 0)];
        while let Some((id, above)) = stack.pop() {
            if self.t.nodes[id as usize].leaf {
                continue;
            }
            let here = above + usize::from(self.operator(id));
            longest = longest.max(here);
            stack.extend(self.ch(id).iter().map(|&c| (c, here)));
        }
        longest
    }

    /// Runs `f` a level deeper, refusing past the bound.
    fn deeper<T>(&self, f: impl FnOnce() -> R<T>) -> R<T> {
        let depth = self.depth.get() + 1;
        if depth > self.bounds.depth {
            return no(TOO_DEEP);
        }
        self.depth.set(depth);
        let out = f();
        self.depth.set(depth - 1);
        out
    }

    fn k(&self, id: NodeId) -> &'static str {
        mysql::kind_name(self.t.node(id).kind)
    }
    fn ch(&self, id: NodeId) -> &'a [NodeId] {
        self.t.children(id)
    }
    fn names(&self, id: NodeId) -> Vec<&'static str> {
        self.ch(id).iter().map(|&c| self.k(c)).collect()
    }
    fn tx(&self, id: NodeId) -> &'a str {
        let n = self.t.node(id);
        &self.t.text[n.start as usize..n.end as usize]
    }
    /// The first child named `name`.
    fn get(&self, id: NodeId, name: &str) -> Option<NodeId> {
        self.ch(id).iter().copied().find(|&c| self.k(c) == name)
    }

    fn empty(&self, id: NodeId) -> bool {
        self.ch(id).is_empty() && !self.t.node(id).leaf
    }
    fn loc(&self, at: usize) -> Location {
        let line = self.lines.partition_point(|&s| s <= at);
        let start = self.lines[line - 1];
        Location::new(line as u64, u64::from(self.chars_before(at) - self.chars_before(start)) + 1)
    }

    /// How many characters stand before byte `at`: the bytes that start one.
    fn chars_before(&self, at: usize) -> u32 {
        let block = at / CHECKPOINT;
        self.chars[block] + self.t.text.as_bytes()[block * CHECKPOINT..at].iter().filter(|&&b| b & 0xC0 != 0x80).count() as u32
    }
    fn span(&self, id: NodeId) -> Span {
        let n = self.t.node(id);
        Span::new(self.loc(n.start as usize), self.loc(n.end as usize))
    }
    /// A list nonterminal `L: X | L ',' X` as its items.
    fn list(&self, id: NodeId, item: &str) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            for &c in self.ch(n).iter().rev() {
                if self.k(c) == self.k(id) {
                    stack.push(c);
                } else if self.k(c) == item {
                    out.push(c);
                }
            }
        }
        out.reverse();
        // The stack visits the left-recursive list's inner node last; restore source order.
        out.sort_by_key(|&n| self.t.node(n).start);
        out
    }

    // ---------------------------------------------------------------- statements

    pub(crate) fn statements(&self) -> R<Vec<Statement>> {
        if let Some(e) = self.t.errors.first() {
            return no(&e.message);
        }
        if self.chain() > self.bounds.chain {
            return no(TOO_LONG_A_CHAIN);
        }
        let mut out = Vec::new();
        let mut stack = vec![self.t.root];
        while let Some(n) = stack.pop() {
            match self.k(n) {
                "Statement" if self.k(self.ch(n)[0]) == "EmptyStmt" => {}
                "Statement" => out.push((self.t.node(n).start, self.statement(n)?)),
                "error" => return no("syntax error"),
                _ => stack.extend(self.ch(n).iter().rev()),
            }
        }
        out.sort_by_key(|s| s.0);
        Ok(out.into_iter().map(|s| s.1).collect())
    }

    fn statement(&self, id: NodeId) -> R<Statement> {
        let c = self.ch(id)[0];
        Ok(match self.k(c) {
            "SubSelect" => Statement::Query(Box::new(plain(SetExpr::Query(Box::new(self.subquery(c)?))))),
            "SelectStmt" | "SetOprStmt" | "SelectStmtWithClause" => Statement::Query(Box::new(self.query(c)?)),
            "InsertIntoStmt" | "ReplaceIntoStmt" => self.insert(c)?,
            "UpdateStmt" => self.update(c)?,
            "DeleteFromStmt" => self.delete(c)?,
            other => return no(&format!("statement {other}")),
        })
    }

    // ---------------------------------------------------------------- queries

    fn query(&self, id: NodeId) -> R<Query> {
        self.deeper(|| self.query_at(id))
    }

    fn query_at(&self, id: NodeId) -> R<Query> {
        match self.k(id) {
            "SelectStmt" => self.select_stmt(id),
            "SubSelect" => self.subquery(id),
            "SelectStmtWithClause" => {
                let with = self.with(self.ch(id)[0])?;
                let mut q = self.query(self.ch(id)[1])?;
                if self.k(self.ch(id)[1]) == "SubSelect" {
                    q = plain(SetExpr::Query(Box::new(q)));
                }
                q.with = Some(with);
                Ok(q)
            }
            "SetOprStmt" => {
                let c = self.ch(id);
                if self.k(c[0]) == "WithClause" {
                    let mut q = self.set_opr(c[1])?;
                    q.with = Some(self.with(c[0])?);
                    Ok(q)
                } else {
                    self.set_opr(c[0])
                }
            }
            other => no(&format!("query {other}")),
        }
    }

    /// A query as the body of a set operation or a subquery: a SELECT with nothing after it is
    /// the SELECT; anything else a nested query.
    fn set_operand(&self, id: NodeId) -> R<SetExpr> {
        let q = match self.k(id) {
            "SetOprClause" => return self.set_operand(self.ch(id)[0]),
            "SubSelect" => return Ok(SetExpr::Query(Box::new(self.subquery(id)?))),
            _ => self.query(id)?,
        };
        Ok(unwrap_plain(q))
    }

    fn set_opr(&self, id: NodeId) -> R<Query> {
        // SetOprStmtWoutLimitOrderBy | SetOprStmtWithLimitOrderBy
        let c = self.ch(id);
        let names = self.names(id);
        let (operands, ops, tail): (Vec<NodeId>, Vec<NodeId>, &[NodeId]) = if names[0] == "SetOprClauseList" {
            let mut operands = Vec::new();
            let mut ops = Vec::new();
            self.flatten_set(c[0], &mut operands, &mut ops);
            ops.push(c[1]);
            operands.push(c[2]);
            (operands, ops, &c[3..])
        } else {
            (vec![c[0]], vec![], &c[1..])
        };
        // The last operand of `… UNION SelectStmt` keeps its own ORDER BY and LIMIT in the grammar,
        // which belong to the whole set operation, as MySQL reads them.
        let mut order_by = None;
        let mut limit = None;
        let mut locks = Vec::new();
        let mut last = operands.len() - 1;
        let mut last_body = None;
        if !ops.is_empty() && self.k(operands[last]) == "SelectStmt" {
            let mut q = self.select_stmt(operands[last])?;
            order_by = q.order_by.take();
            limit = q.limit_clause.take();
            locks = std::mem::take(&mut q.locks);
            last_body = Some(*q.body);
        }
        let mut body: Option<SetExpr> = None;
        let mut pending: Vec<(SetOperator, SetQuantifier, SetExpr)> = Vec::new();
        for (i, &o) in operands.iter().enumerate() {
            let e = if i == last { match last_body.take() { Some(b) => b, None => self.set_operand(o)? } } else { self.set_operand(o)? };
            if i == 0 {
                body = Some(e);
            } else {
                let (op, q) = self.set_op(ops[i - 1]);
                pending.push((op, q, e));
            }
        }
        let _ = &mut last;
        let mut body = combine(body.unwrap(), pending);
        if operands.len() == 1 && ops.is_empty() {
            // SubSelect OrderBy / SelectStmtLimit: the parenthesized query is the body.
            body = match body {
                SetExpr::Query(q) => SetExpr::Query(q),
                b => b,
            };
        }
        let mut q = plain(body);
        q.order_by = order_by;
        q.limit_clause = limit;
        q.locks = locks;
        for &t in tail {
            match self.k(t) {
                "OrderBy" => q.order_by = Some(self.order_by(t)?),
                "SelectStmtLimit" => q.limit_clause = Some(self.limit(t)?),
                "\"INTO\"" => {}
                "SelectIntoTarget" => {
                    // The INTO of a parenthesized query is its SELECT's.
                    let into = self.into(t)?;
                    let mut body = q.body.as_mut();
                    loop {
                        match body {
                            SetExpr::Query(inner) => body = inner.body.as_mut(),
                            SetExpr::Select(s) => {
                                s.into = Some(into);
                                break;
                            }
                            _ => return no("INTO after a set operation"),
                        }
                    }
                }
                other => return no(&format!("set tail {other}")),
            }
        }
        Ok(q)
    }

    fn flatten_set(&self, id: NodeId, operands: &mut Vec<NodeId>, ops: &mut Vec<NodeId>) {
        let c = self.ch(id);
        if c.len() == 1 {
            operands.push(c[0]);
        } else {
            self.flatten_set(c[0], operands, ops);
            ops.push(c[1]);
            operands.push(c[2]);
        }
    }

    fn set_op(&self, id: NodeId) -> (SetOperator, SetQuantifier) {
        let c = self.ch(id);
        let op = match self.tx(c[0]).to_ascii_uppercase().as_str() {
            "UNION" => SetOperator::Union,
            "EXCEPT" => SetOperator::Except,
            _ => SetOperator::Intersect,
        };
        let q = match c.get(1).map(|&o| self.tx(o).to_ascii_uppercase()) {
            Some(w) if w == "ALL" => SetQuantifier::All,
            Some(w) if w == "DISTINCT" => SetQuantifier::Distinct,
            _ => SetQuantifier::None,
        };
        (op, q)
    }

    fn with(&self, id: NodeId) -> R<With> {
        let recursive = self.ch(id).len() == 3;
        let list = *self.ch(id).last().unwrap();
        let mut cte_tables = Vec::new();
        for cte in self.list(list, "CommonTableExpr") {
            let c = self.ch(cte);
            let name = self.ident(c[0])?;
            let columns = self.ident_list_paren(c[1])?.into_iter().map(|name| TableAliasColumnDef { name, data_type: None }).collect();
            let query = self.subquery(c[3])?;
            cte_tables.push(Cte { alias: TableAlias { explicit: false, name, columns, at: None }, query: Box::new(query), from: None, materialized: None, closing_paren_token: AttachedToken::empty() });
        }
        Ok(With { with_token: AttachedToken::empty(), recursive, cte_tables })
    }

    fn ident_list_paren(&self, id: NodeId) -> R<Vec<Ident>> {
        // IdentListWithParenOpt: | '(' IdentList ')'
        let mut out = Vec::new();
        if let Some(l) = self.get(id, "IdentList") {
            for i in self.list(l, "Identifier") {
                out.push(self.ident(i)?);
            }
        }
        Ok(out)
    }

    /// A SubSelect's query, as a subquery reads it: `(SELECT …)` is the SELECT's query.
    fn subquery(&self, id: NodeId) -> R<Query> {
        match self.k(id) {
            "SubSelect" => {
                let inner = self.ch(id)[1];
                if self.k(inner) == "SubSelect" {
                    return Ok(plain(SetExpr::Query(Box::new(self.subquery(inner)?))));
                }
                self.query(inner)
            }
            _ => self.query(id),
        }
    }

    fn select_stmt(&self, id: NodeId) -> R<Query> {
        let c = self.ch(id);
        let first = c[0];
        let mut select;
        let rest: &[NodeId];
        match self.k(first) {
            "SelectStmtBasic" => {
                select = self.select_basic(first)?;
                rest = &c[1..];
            }
            "SelectStmtFromDualTable" => {
                let d = self.ch(first);
                select = self.select_basic(d[0])?;
                let dual = self.ch(d[1])[1];
                select.from = vec![TableWithJoins { relation: TableFactor::Table { name: ObjectName::from(vec![Ident::new(self.tx(dual))]), alias: None, args: None, with_hints: vec![], version: None, with_ordinality: false, partitions: vec![], json_path: None, sample: None, index_hints: vec![] }, joins: vec![] }];
                if let Some(w) = self.where_clause(d[2])? {
                    select.selection = Some(w);
                }
                rest = &c[1..];
            }
            "SelectStmtFromTable" => {
                let d = self.ch(first);
                select = self.select_basic(d[0])?;
                select.from = self.table_refs(d[2])?;
                select.selection = self.where_clause(d[3])?;
                self.group(d[4], &mut select)?;
                select.having = self.having(d[5])?;
                select.named_window = self.windows(d[6])?;
                select.window_before_qualify = !select.named_window.is_empty();
                if let [_, e] = self.ch(d[7]) {
                    select.qualify = Some(self.expr(*e)?);
                }
                rest = &c[1..];
            }
            "\"TABLE\"" => {
                let name = self.table_name(c[1])?;
                let parts: Vec<String> = name.0.iter().map(|p| p.to_string()).collect();
                let (schema_name, table_name) = if parts.len() == 2 { (Some(parts[0].clone()), Some(parts[1].clone())) } else { (None, Some(parts[0].clone())) };
                let mut q = plain(SetExpr::Table(Box::new(Table { table_name, schema_name })));
                self.query_tail(&c[2..], &mut q)?;
                return Ok(q);
            }
            "\"VALUES\"" => {
                let mut rows = Vec::new();
                for r in self.list(c[1], "RowStmt") {
                    rows.push(self.row(self.ch(r)[1])?);
                }
                let mut q = plain(SetExpr::Values(Values { explicit_row: true, value_keyword: false, rows }));
                self.query_tail(&c[2..], &mut q)?;
                return Ok(q);
            }
            other => return no(&format!("select {other}")),
        }
        let mut q = plain(SetExpr::Select(Box::new(Select { ..select })));
        for &r in rest {
            match self.k(r) {
                "WhereClauseOptional" => {
                    if let Some(w) = self.where_clause(r)? {
                        sel(&mut q).selection = Some(w);
                    }
                }
                "SelectStmtGroup" => self.group(r, sel(&mut q))?,
                "HavingClause" => sel(&mut q).having = self.having(r)?,
                "OrderByOptional" => {
                    if let Some(o) = self.get(r, "OrderBy") {
                        q.order_by = Some(self.order_by(o)?);
                    }
                }
                "SelectStmtLimitOpt" => {
                    if let Some(l) = self.get(r, "SelectStmtLimit") {
                        q.limit_clause = Some(self.limit(l)?);
                    }
                }
                "SelectLockOpt" => {
                    if !self.empty(r) {
                        q.locks = self.lock(r)?;
                    }
                }
                "SelectStmtIntoOption" => {
                    if !self.empty(r) {
                        sel(&mut q).into = Some(self.into(self.ch(r)[1])?);
                        if let Some(&l) = self.ch(r).get(2) {
                            q.locks.extend(self.lock(l)?);
                        }
                    }
                }
                other => return no(&format!("select tail {other}")),
            }
        }
        Ok(q)
    }

    /// ORDER BY, LIMIT, locking and INTO after a TABLE or VALUES statement.
    fn query_tail(&self, rest: &[NodeId], q: &mut Query) -> R<()> {
        for &r in rest {
            match self.k(r) {
                "OrderByOptional" => {
                    if let Some(o) = self.get(r, "OrderBy") {
                        q.order_by = Some(self.order_by(o)?);
                    }
                }
                "SelectStmtLimitOpt" => {
                    if let Some(l) = self.get(r, "SelectStmtLimit") {
                        q.limit_clause = Some(self.limit(l)?);
                    }
                }
                "SelectLockOpt" => {
                    if !self.empty(r) {
                        q.locks = self.lock(r)?;
                    }
                }
                "SelectStmtIntoOption" => {
                    if !self.empty(r) {
                        return no("TABLE … INTO");
                    }
                }
                other => return no(&format!("tail {other}")),
            }
        }
        Ok(())
    }

    fn select_basic(&self, id: NodeId) -> R<Select> {
        // "SELECT" SelectStmtOpts SelectStmtFieldList SelectIntoEarly
        let c = self.ch(id);
        let mut s = empty_select();
        self.select_opts(c[1], &mut s)?;
        let fields = self.ch(c[2])[0];
        for f in self.list(fields, "Field") {
            s.projection.push(self.field(f)?);
        }
        if !self.empty(c[3]) {
            s.into = Some(self.into(self.ch(c[3])[1])?);
        }
        Ok(s)
    }

    fn select_opts(&self, id: NodeId, s: &mut Select) -> R<()> {
        if self.empty(id) {
            return Ok(());
        }
        let mut words = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if self.t.node(n).leaf {
                words.push((self.t.node(n).start, n));
            } else {
                stack.extend(self.ch(n).iter().copied());
            }
        }
        words.sort();
        let mut m = SelectModifiers::default();
        let mut any = false;
        for (_, w) in words {
            let text = self.tx(w);
            match text.to_ascii_uppercase().as_str() {
                "DISTINCT" | "DISTINCTROW" => s.distinct = Some(Distinct::Distinct),
                "ALL" => s.distinct = Some(Distinct::All),
                "HIGH_PRIORITY" => { m.high_priority = true; any = true }
                "STRAIGHT_JOIN" => { m.straight_join = true; any = true }
                "SQL_SMALL_RESULT" => { m.sql_small_result = true; any = true }
                "SQL_BIG_RESULT" => { m.sql_big_result = true; any = true }
                "SQL_BUFFER_RESULT" => { m.sql_buffer_result = true; any = true }
                "SQL_NO_CACHE" => { m.sql_no_cache = true; any = true }
                "SQL_CALC_FOUND_ROWS" => { m.sql_calc_found_rows = true; any = true }
                "SQL_CACHE" => {}
                _ if text.starts_with("/*+") => s.optimizer_hints.push(OptimizerHint { prefix: String::new(), text: text[3..text.len() - 2].to_string(), style: OptimizerHintStyle::MultiLine }),
                other => return no(&format!("select option {other}")),
            }
        }
        if any {
            s.select_modifiers = Some(m);
        }
        Ok(())
    }

    fn field(&self, id: NodeId) -> R<SelectItem> {
        let c = self.ch(id);
        match self.names(id).as_slice() {
            ["'*'"] => Ok(SelectItem::Wildcard(WildcardAdditionalOptions::default())),
            ["Identifier", "'.'", "'*'"] => Ok(SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(ObjectName::from(vec![self.ident(c[0])?])), WildcardAdditionalOptions::default())),
            ["Identifier", "'.'", "Identifier", "'.'", "'*'"] => Ok(SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(ObjectName::from(vec![self.ident(c[0])?, self.ident(c[2])?])), WildcardAdditionalOptions::default())),
            ["Expression", "FieldAsNameOpt"] => {
                let expr = self.expr(c[0])?;
                match self.get(c[1], "FieldAsName") {
                    None => Ok(SelectItem::UnnamedExpr(expr)),
                    Some(a) => {
                        let last = *self.ch(a).last().unwrap();
                        let alias = if self.k(last) == "Identifier" { self.ident(last)? } else { self.string_ident(last) };
                        Ok(SelectItem::ExprWithAlias { expr, alias })
                    }
                }
            }
            other => no(&format!("field {other:?}")),
        }
    }

    fn into(&self, id: NodeId) -> R<SelectInto> {
        // SelectIntoTarget: "OUTFILE" … | "DUMPFILE" … | SelectIntoVars
        let c = self.ch(id);
        match self.k(c[0]) {
            "SelectIntoVars" => {
                let mut targets = Vec::new();
                self.collect_vars(c[0], &mut targets)?;
                Ok(SelectInto { temporary: false, unlogged: false, table: false, targets })
            }
            _ => no("INTO OUTFILE"),
        }
    }

    fn collect_vars(&self, id: NodeId, out: &mut Vec<Expr>) -> R<()> {
        for &c in self.ch(id) {
            match self.k(c) {
                "','" => {}
                "SelectIntoVars" | "SelectIntoVar" => self.collect_vars(c, out)?,
                _ => out.push(self.expr_any(c)?),
            }
        }
        Ok(())
    }

    fn where_clause(&self, id: NodeId) -> R<Option<Expr>> {
        match self.get(id, "WhereClause") {
            Some(w) => Ok(Some(self.expr(self.ch(w)[1])?)),
            None => Ok(None),
        }
    }

    fn having(&self, id: NodeId) -> R<Option<Expr>> {
        if self.empty(id) {
            return Ok(None);
        }
        Ok(Some(self.expr(self.ch(id)[1])?))
    }

    fn group(&self, id: NodeId, s: &mut Select) -> R<()> {
        let Some(g) = self.get(id, "GroupByClause") else { return Ok(()) };
        let c = self.ch(g);
        if c.len() == 6 {
            // GROUP BY ROLLUP(…) is GROUP BY … WITH ROLLUP.
            let mut exprs = Vec::new();
            for item in self.list(c[4], "ByItem") {
                exprs.push(self.expr(self.ch(item)[0])?);
            }
            s.group_by = GroupByExpr::Expressions(exprs, vec![GroupByWithModifier::Rollup]);
            return Ok(());
        }
        let mut exprs = Vec::new();
        for item in self.list(c[2], "ByItem") {
            let ic = self.ch(item);
            if ic.len() > 1 {
                return no("GROUP BY with ASC/DESC");
            }
            exprs.push(self.expr(ic[0])?);
        }
        let mut modifiers = Vec::new();
        if !self.empty(c[3]) {
            modifiers.push(GroupByWithModifier::Rollup);
        }
        s.group_by = GroupByExpr::Expressions(exprs, modifiers);
        Ok(())
    }

    fn windows(&self, id: NodeId) -> R<Vec<NamedWindowDefinition>> {
        let mut out = Vec::new();
        if let Some(l) = self.get(id, "WindowDefinitionList") {
            for d in self.list(l, "WindowDefinition") {
                let c = self.ch(d);
                out.push(NamedWindowDefinition(self.ident(c[0])?, NamedWindowExpr::WindowSpec(self.window_spec(c[2])?)));
            }
        }
        Ok(out)
    }

    fn order_by(&self, id: NodeId) -> R<OrderBy> {
        Ok(OrderBy { kind: OrderByKind::Expressions(self.by_list(self.ch(id)[2])?), interpolate: None })
    }

    fn by_list(&self, id: NodeId) -> R<Vec<OrderByExpr>> {
        let mut out = Vec::new();
        for item in self.list(id, "ByItem") {
            let ic = self.ch(item);
            let sort = match ic.get(1).map(|&o| self.tx(o).to_ascii_uppercase()) {
                Some(w) if w == "ASC" => Some(OrderBySort::Asc),
                Some(w) if w == "DESC" => Some(OrderBySort::Desc),
                _ => None,
            };
            out.push(OrderByExpr { expr: self.expr(ic[0])?, options: OrderByOptions { sort, nulls_first: None }, with_fill: None });
        }
        Ok(out)
    }

    fn limit(&self, id: NodeId) -> R<LimitClause> {
        let c = self.ch(id);
        match self.names(id).as_slice() {
            ["\"LIMIT\"", "LimitOption"] => Ok(LimitClause::LimitOffset { limit: Some(self.limit_option(c[1])?), offset: None, limit_by: vec![] }),
            ["\"LIMIT\"", "LimitOption", "','", "LimitOption"] => Ok(LimitClause::OffsetCommaLimit { offset: self.limit_option(c[1])?, limit: self.limit_option(c[3])? }),
            ["\"LIMIT\"", "LimitOption", "\"OFFSET\"", "LimitOption"] => Ok(LimitClause::LimitOffset { limit: Some(self.limit_option(c[1])?), offset: Some(Offset { value: self.limit_option(c[3])?, rows: OffsetRows::None }), limit_by: vec![] }),
            other => no(&format!("limit {other:?}")),
        }
    }

    fn limit_option(&self, id: NodeId) -> R<Expr> {
        self.expr_any(id)
    }

    fn lock(&self, id: NodeId) -> R<Vec<LockClause>> {
        let mut out = Vec::new();
        let list = if self.k(id) == "SelectLockList" { id } else { self.ch(id)[0] };
        for l in self.list(list, "SelectLock") {
            out.extend(self.lock_one(l)?);
        }
        Ok(out)
    }

    fn lock_one(&self, id: NodeId) -> R<Vec<LockClause>> {
        let c = self.ch(id);
        let words: Vec<String> = c.iter().map(|&n| self.tx(n).to_ascii_uppercase()).collect();
        if words[0] == "LOCK" {
            return Ok(vec![LockClause { lock_type: LockType::Share, of: vec![], nonblock: None }]);
        }
        let lock_type = if words[1] == "UPDATE" { LockType::Update } else { LockType::Share };
        let mut of = Vec::new();
        if let Some(t) = self.get(id, "OfTablesOpt") {
            if let Some(l) = self.get(t, "TableNameList") {
                for n in self.list(l, "TableName") {
                    of.push(self.table_name(n)?);
                }
            }
        }
        let nonblock = if words.iter().any(|w| w == "NOWAIT") {
            Some(NonBlock::Nowait)
        } else if words.iter().any(|w| w == "SKIP") {
            Some(NonBlock::SkipLocked)
        } else {
            None
        };
        if words.iter().any(|w| w == "WAIT") {
            return no("FOR UPDATE WAIT");
        }
        Ok(vec![LockClause { lock_type, of, nonblock }])
    }

    // ---------------------------------------------------------------- tables

    fn table_refs(&self, id: NodeId) -> R<Vec<TableWithJoins>> {
        let refs = if self.k(id) == "TableRefsClause" { self.ch(id)[0] } else { id };
        let mut out = Vec::new();
        for e in self.list(refs, "EscapedTableRef") {
            let c = self.ch(e);
            if c.len() != 1 {
                return no("{ OJ }");
            }
            out.push(self.table_with_joins(c[0])?);
        }
        Ok(out)
    }

    fn table_with_joins(&self, id: NodeId) -> R<TableWithJoins> {
        self.deeper(|| self.table_with_joins_at(id))
    }

    fn table_with_joins_at(&self, id: NodeId) -> R<TableWithJoins> {
        // TableRef: TableFactor | JoinTable
        let c = self.ch(id)[0];
        match self.k(c) {
            "TableFactor" => Ok(TableWithJoins { relation: self.table_factor(c)?, joins: vec![] }),
            "JoinTable" => {
                let jc = self.ch(c);
                let mut left = self.table_with_joins(jc[0])?;
                // `a JOIN b JOIN c ON x`: the grammar nests the joins after a join with no ON or
                // USING of its own in its right operand, which MySQL reads as the left operand
                // of those joins instead: `(a JOIN b) JOIN c ON x`.
                let names = self.names(c);
                let bare = names.len() == 3 && (names[1] == "CrossOpt" || names[1] == "\"STRAIGHT_JOIN\"");
                let right = jc[2];
                if bare && self.k(self.ch(right)[0]) == "JoinTable" {
                    let mut joins = self.join_chain(c)?;
                    left.joins.append(&mut joins);
                    return Ok(left);
                }
                let join = self.join(c)?;
                left.joins.push(join);
                Ok(left)
            }
            other => no(&format!("table ref {other}")),
        }
    }

    /// The joins of a bare join whose right operand is itself a join, each the left operand of the next.
    fn join_chain(&self, id: NodeId) -> R<Vec<Join>> {
        let c = self.ch(id);
        let right_join = self.ch(c[2])[0];
        let rc = self.ch(right_join);
        // The first join takes the right operand's leftmost table with this join's own operator.
        let inner = self.table_with_joins(rc[0])?;
        let mut out = Vec::new();
        let op = self.join_operator_bare(c[1]);
        if inner.joins.is_empty() {
            out.push(Join { relation: inner.relation, global: false, join_operator: op });
        } else {
            out.push(Join { relation: TableFactor::NestedJoin { table_with_joins: Box::new(inner), alias: None }, global: false, join_operator: op });
        }
        let rn = self.names(right_join);
        if rn.len() == 3 && (rn[1] == "CrossOpt" || rn[1] == "\"STRAIGHT_JOIN\"") && self.k(self.ch(rc[2])[0]) == "JoinTable" {
            out.extend(self.join_chain(right_join)?);
        } else {
            out.push(self.join(right_join)?);
        }
        Ok(out)
    }

    fn join_operator_bare(&self, id: NodeId) -> JoinOperator {
        match self.k(id) {
            "\"STRAIGHT_JOIN\"" => JoinOperator::StraightJoin(JoinConstraint::None),
            _ => match self.tx(self.ch(id)[0]).to_ascii_uppercase().as_str() {
                "CROSS" => JoinOperator::CrossJoin(JoinConstraint::None),
                "INNER" => JoinOperator::Inner(JoinConstraint::None),
                _ => JoinOperator::Join(JoinConstraint::None),
            },
        }
    }

    fn join(&self, id: NodeId) -> R<Join> {
        let c = self.ch(id);
        let names = self.names(id);
        let right = |at: usize| -> R<TableFactor> {
            let r = self.table_with_joins(c[at])?;
            if r.joins.is_empty() {
                Ok(r.relation)
            } else {
                Ok(TableFactor::NestedJoin { table_with_joins: Box::new(r), alias: None })
            }
        };
        let constraint = |from: usize| -> R<JoinConstraint> {
            match names.get(from) {
                None => Ok(JoinConstraint::None),
                Some(&"\"ON\"") => Ok(JoinConstraint::On(self.expr(c[from + 1])?)),
                Some(&"\"USING\"") => {
                    let mut cols = Vec::new();
                    for n in self.list(c[from + 2], "ColumnName") {
                        cols.push(ObjectName::from(self.column_name(n)?));
                    }
                    Ok(JoinConstraint::Using(cols))
                }
                Some(other) => no(&format!("join constraint {other}")),
            }
        };
        let operator = match names[1] {
            "CrossOpt" => {
                let w = self.tx(self.ch(c[1])[0]).to_ascii_uppercase();
                let k = constraint(3)?;
                match w.as_str() {
                    "CROSS" => {
                        if matches!(k, JoinConstraint::None) {
                            JoinOperator::CrossJoin(JoinConstraint::None)
                        } else {
                            JoinOperator::CrossJoin(k)
                        }
                    }
                    "INNER" => JoinOperator::Inner(k),
                    _ => JoinOperator::Join(k),
                }
            }
            "JoinType" => {
                let w = self.tx(self.ch(c[1])[0]).to_ascii_uppercase();
                let outer = !self.empty(c[2]);
                let k = constraint(5)?;
                match (w.as_str(), outer) {
                    ("LEFT", false) => JoinOperator::Left(k),
                    ("LEFT", true) => JoinOperator::LeftOuter(k),
                    ("RIGHT", false) => JoinOperator::Right(k),
                    ("RIGHT", true) => JoinOperator::RightOuter(k),
                    _ => JoinOperator::FullOuter(k),
                }
            }
            "\"NATURAL\"" => {
                if names[2] == "\"JOIN\"" {
                    return Ok(Join { relation: right(3)?, global: false, join_operator: JoinOperator::Join(JoinConstraint::Natural) });
                }
                if names[2] == "\"INNER\"" {
                    return Ok(Join { relation: right(4)?, global: false, join_operator: JoinOperator::Inner(JoinConstraint::Natural) });
                }
                let w = self.tx(self.ch(c[2])[0]).to_ascii_uppercase();
                let outer = !self.empty(c[3]);
                let relation = right(5)?;
                let op = match (w.as_str(), outer) {
                    ("LEFT", false) => JoinOperator::Left(JoinConstraint::Natural),
                    ("LEFT", true) => JoinOperator::LeftOuter(JoinConstraint::Natural),
                    ("RIGHT", false) => JoinOperator::Right(JoinConstraint::Natural),
                    ("RIGHT", true) => JoinOperator::RightOuter(JoinConstraint::Natural),
                    _ => JoinOperator::FullOuter(JoinConstraint::Natural),
                };
                return Ok(Join { relation, global: false, join_operator: op });
            }
            "\"STRAIGHT_JOIN\"" => JoinOperator::StraightJoin(constraint(3)?),
            other => return no(&format!("join {other}")),
        };
        let at = if names[1] == "JoinType" { 4 } else { 2 };
        Ok(Join { relation: right(at)?, global: false, join_operator: operator })
    }

    fn table_factor(&self, id: NodeId) -> R<TableFactor> {
        let c = self.ch(id);
        match self.names(id).as_slice() {
            ["TableName", "PartitionNameListOpt", "TableAsNameOpt", "AsOfClauseOpt", "IndexHintListOpt", "TableSampleOpt"] => {
                let mut partitions = Vec::new();
                if let Some(l) = self.get(c[1], "PartitionNameList") {
                    for i in self.list(l, "Identifier") {
                        partitions.push(self.ident(i)?);
                    }
                }
                if !self.empty(c[3]) || !self.empty(c[5]) {
                    return no("AS OF / TABLESAMPLE");
                }
                let index_hints = self.index_hints(c[4])?;
                Ok(TableFactor::Table { name: self.table_name(c[0])?, alias: self.table_alias(c[2])?, args: None, with_hints: vec![], version: None, with_ordinality: false, partitions, json_path: None, sample: None, index_hints })
            }
            ["SubSelect", "TableAsName", "'('", "IdentList", "')'"] => {
                let mut alias = self.table_alias_name(c[1])?;
                for i in self.list(c[3], "Identifier") {
                    alias.columns.push(TableAliasColumnDef { name: self.ident(i)?, data_type: None });
                }
                Ok(TableFactor::Derived { lateral: false, subquery: Box::new(self.subquery(c[0])?), alias: Some(alias), sample: None })
            }
            ["SubSelect", "TableAsNameOpt"] => Ok(TableFactor::Derived { lateral: false, subquery: Box::new(self.subquery(c[0])?), alias: self.table_alias(c[1])?, sample: None }),
            ["\"LATERAL\"", "SubSelect", "TableAsName", "IdentListWithParenOpt"] => {
                let mut alias = self.table_alias_name(c[2])?;
                alias.columns = self.ident_list_paren(c[3])?.into_iter().map(|name| TableAliasColumnDef { name, data_type: None }).collect();
                Ok(TableFactor::Derived { lateral: true, subquery: Box::new(self.subquery(c[1])?), alias: Some(alias), sample: None })
            }
            ["'('", "TableRefs", "')'"] => {
                let refs = comma_join(self.table_refs(c[1])?);
                // A lone table may stand in parentheses, and is that table.
                if refs.joins.is_empty() && matches!(refs.relation, TableFactor::Table { .. }) {
                    return Ok(refs.relation);
                }
                Ok(TableFactor::NestedJoin { table_with_joins: Box::new(refs), alias: None })
            }
            ["JsonTable"] => self.json_table(c[0]),
            other => no(&format!("table factor {other:?}")),
        }
    }

    fn json_table(&self, id: NodeId) -> R<TableFactor> {
        let c = self.ch(id);
        let Expr::Value(json_path) = self.string_literal(c[4]) else { unreachable!() };
        Ok(TableFactor::JsonTable { json_expr: self.expr(c[2])?, json_path, columns: self.json_columns(c[7])?, alias: self.table_alias(c[10])? })
    }

    fn json_columns(&self, id: NodeId) -> R<Vec<JsonTableColumn>> {
        let mut out = Vec::new();
        for col in self.list(id, "JsonTableColumn") {
            let c = self.ch(col);
            let names = self.names(col);
            let path = |n: NodeId| -> ValueWithSpan {
                let Expr::Value(v) = self.string_literal(n) else { unreachable!() };
                v
            };
            out.push(match names.as_slice() {
                ["Identifier", "\"FOR\"", "\"ORDINALITY\""] => JsonTableColumn::ForOrdinality(self.ident(c[0])?),
                ["Identifier", "RoutineType", "\"PATH\"", "\"string literal\"", "JsonTableOn"] | ["Identifier", "RoutineType", "\"EXISTS\"", "\"PATH\"", "\"string literal\""] => {
                    let exists = names[2] == "\"EXISTS\"";
                    let (r#type, character_set, mut collation) = self.data_type(self.ch(c[1])[0])?;
                    if let [_, _, n] = self.ch(c[1]) {
                        collation = Some(ObjectName::from(vec![self.name_ident(*n)]));
                    }
                    let (on_empty, on_error) = if exists { (None, None) } else { self.json_on(c[4])? };
                    JsonTableColumn::Named(JsonTableNamedColumn { name: self.ident(c[0])?, r#type, character_set, collation, path: path(c[if exists { 4 } else { 3 }]), exists, on_empty, on_error })
                }
                ["\"NESTED\"", "\"PATH\"", "\"string literal\"", "\"COLUMNS\"", "'('", "JsonTableColumns", "')'"] => JsonTableColumn::Nested(JsonTableNestedColumn { path: path(c[2]), columns: self.json_columns(c[5])? }),
                ["\"NESTED\"", "\"string literal\"", "\"COLUMNS\"", "'('", "JsonTableColumns", "')'"] => JsonTableColumn::Nested(JsonTableNestedColumn { path: path(c[1]), columns: self.json_columns(c[4])? }),
                other => return no(&format!("json_table column {other:?}")),
            });
        }
        Ok(out)
    }

    fn json_on(&self, id: NodeId) -> R<(Option<JsonTableColumnErrorHandling>, Option<JsonTableColumnErrorHandling>)> {
        let c = self.ch(id);
        let response = |n: NodeId| -> JsonTableColumnErrorHandling {
            let rc = self.ch(n);
            match self.tx(rc[0]).to_ascii_uppercase().as_str() {
                "NULL" => JsonTableColumnErrorHandling::Null,
                "ERROR" => JsonTableColumnErrorHandling::Error,
                _ => {
                    let Expr::Value(v) = self.string_literal(rc[1]) else { unreachable!() };
                    JsonTableColumnErrorHandling::Default(v)
                }
            }
        };
        // Each response by the word after its ON, in either order.
        let (mut on_empty, mut on_error) = (None, None);
        for at in (0..c.len()).step_by(3) {
            if self.tx(c[at + 2]).eq_ignore_ascii_case("EMPTY") {
                on_empty = Some(response(c[at]));
            } else {
                on_error = Some(response(c[at]));
            }
        }
        Ok((on_empty, on_error))
    }

    /// A column type, its character set and its collation.
    fn data_type(&self, id: NodeId) -> R<(DataType, Option<ObjectName>, Option<ObjectName>)> {
        let t = self.ch(id)[0];
        let c = self.ch(t);
        let names = self.names(t);
        let word = |n: NodeId| self.tx(n).to_ascii_uppercase();
        let len = |n: NodeId| -> Option<u64> {
            if self.k(n) == "FieldLen" {
                return Some(self.tx(self.ch(n)[1]).parse().unwrap_or(0));
            }
            self.get(n, "FieldLen").map(|f| self.tx(self.ch(f)[1]).parse().unwrap_or(0))
        };
        let opts = |n: NodeId| -> (bool, bool) {
            let w = self.tx(n).to_ascii_uppercase();
            (w.contains("UNSIGNED"), w.contains("ZEROFILL"))
        };
        let char_len = |l: Option<u64>| l.map(|length| CharacterLength::IntegerLength { length, unit: None });
        let mut charset = None;
        let ty = match (self.k(t), names.as_slice()) {
            ("NumericType", ["IntegerType", "OptFieldLen", "FieldOpts"]) => {
                let l = len(c[1]);
                let (unsigned, zerofill) = opts(c[2]);
                if zerofill {
                    return no("ZEROFILL");
                }
                match (word(c[0]).as_str(), unsigned) {
                    ("TINYINT" | "INT1", false) => DataType::TinyInt(l),
                    ("TINYINT" | "INT1", true) => DataType::TinyIntUnsigned(l),
                    ("SMALLINT" | "INT2", false) => DataType::SmallInt(l),
                    ("SMALLINT" | "INT2", true) => DataType::SmallIntUnsigned(l),
                    ("MEDIUMINT" | "MIDDLEINT" | "INT3", false) => DataType::MediumInt(l),
                    ("MEDIUMINT" | "MIDDLEINT" | "INT3", true) => DataType::MediumIntUnsigned(l),
                    ("INT", false) => DataType::Int(l),
                    ("INT", true) => DataType::IntUnsigned(l),
                    ("INT4", false) => DataType::Int4(l),
                    ("INT4", true) => DataType::Int4Unsigned(l),
                    ("INTEGER", false) => DataType::Integer(l),
                    ("INTEGER", true) => DataType::IntegerUnsigned(l),
                    ("BIGINT" | "INT8", false) => DataType::BigInt(l),
                    ("BIGINT" | "INT8", true) => DataType::BigIntUnsigned(l),
                    _ => return no("integer type"),
                }
            }
            ("NumericType", ["BooleanType", "FieldOpts"]) => {
                if word(c[0]) == "BOOL" {
                    DataType::Bool
                } else {
                    DataType::Boolean
                }
            }
            ("NumericType", ["FixedPointType", "FloatOpt", "FieldOpts"]) => {
                let info = self.number_info(c[1]);
                let (unsigned, _) = opts(c[2]);
                match (word(c[0]).as_str(), unsigned) {
                    ("DECIMAL", false) => DataType::Decimal(info),
                    ("DECIMAL", true) => DataType::DecimalUnsigned(info),
                    ("NUMERIC", _) => DataType::Numeric(info),
                    _ => DataType::Dec(info),
                }
            }
            ("NumericType", ["FloatingPointType", "FloatOpt", "FieldOpts"]) => {
                let info = self.number_info(c[1]);
                let (unsigned, _) = opts(c[2]);
                let w = self.tx(c[0]).to_ascii_uppercase().split_whitespace().collect::<Vec<_>>().join(" ");
                match (w.as_str(), unsigned) {
                    ("FLOAT", false) => DataType::Float(info),
                    ("FLOAT", true) => DataType::FloatUnsigned(info),
                    ("REAL", false) => DataType::Real,
                    ("REAL", true) => DataType::RealUnsigned,
                    ("DOUBLE", false) => DataType::Double(info),
                    ("DOUBLE", true) => DataType::DoubleUnsigned(info),
                    ("DOUBLE PRECISION", false) => DataType::DoublePrecision,
                    ("DOUBLE PRECISION", true) => DataType::DoublePrecisionUnsigned,
                    ("FLOAT4", _) => DataType::Float4,
                    ("FLOAT8", _) => DataType::Float8,
                    _ => return no("float type"),
                }
            }
            ("NumericType", ["BitValueType", "OptFieldLen"]) => DataType::Bit(len(c[1])),
            ("StringType", ["Char", "FieldLen", "OptBinary"] | ["Char", "OptBinary"]) => {
                charset = self.opt_binary(*c.last().unwrap())?;
                DataType::Char(char_len(if c.len() == 3 { len(c[1]) } else { None }))
            }
            ("StringType", ["NChar", rest @ ..]) => {
                let l = if rest.len() == 2 { len(c[1]) } else { None };
                DataType::Custom(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), l.map(|n| vec![n.to_string()]).unwrap_or_default())
            }
            ("StringType", ["Varchar", "FieldLen", "OptBinary"]) => {
                charset = self.opt_binary(c[2])?;
                DataType::Varchar(char_len(len(c[1])))
            }
            ("StringType", ["NVarchar", "FieldLen", "OptBinary"]) => DataType::Nvarchar(char_len(len(c[1]))),
            ("StringType", ["\"BINARY\"", "OptFieldLen"]) => DataType::Binary(len(c[1])),
            ("StringType", ["\"VARBINARY\"", "FieldLen"]) => DataType::Varbinary(len(c[1]).map(|length| BinaryLength::IntegerLength { length })),
            ("StringType", ["BlobType"]) => {
                let b = self.ch(c[0]);
                match word(b[0]).as_str() {
                    "TINYBLOB" => DataType::TinyBlob,
                    "BLOB" => DataType::Blob(len(b[1])),
                    "MEDIUMBLOB" => DataType::MediumBlob,
                    "LONGBLOB" => DataType::LongBlob,
                    _ => return no("long varbinary"),
                }
            }
            ("StringType", ["TextType", "OptCharsetWithOptBinary"]) => {
                let tc = self.ch(c[0]);
                if let Some(&b) = self.ch(c[1]).first() {
                    charset = self.opt_binary(b)?;
                }
                match word(tc[0]).as_str() {
                    "TINYTEXT" => DataType::TinyText,
                    "TEXT" => DataType::Text,
                    "MEDIUMTEXT" => DataType::MediumText,
                    _ => DataType::LongText,
                }
            }
            ("StringType", ["\"ENUM\"" | "\"SET\"", "'('", "TextStringList", "')'", "OptCharsetWithOptBinary"]) => {
                let mut values = Vec::new();
                for v in self.list(c[2], "TextString") {
                    let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(text) | Value::DoubleQuotedString(text), .. }) = self.string_literal(self.ch(v)[0]) else { return no("enum member") };
                    values.push(text);
                }
                if let Some(&b) = self.ch(c[4]).first() {
                    charset = self.opt_binary(b)?;
                }
                if word(c[0]) == "ENUM" {
                    DataType::Enum(values.into_iter().map(EnumMember::Name).collect(), None)
                } else {
                    DataType::Set(values)
                }
            }
            ("StringType", ["\"JSON\""]) => DataType::JSON,
            ("DateAndTimeType", ["\"DATE\""]) => DataType::Date,
            ("DateAndTimeType", ["\"DATETIME\"", "OptFieldLen"]) => DataType::Datetime(len(c[1])),
            ("DateAndTimeType", ["\"TIMESTAMP\"", "OptFieldLen"]) => DataType::Timestamp(len(c[1]), TimezoneInfo::None),
            ("DateAndTimeType", ["\"TIME\"", "OptFieldLen"]) => DataType::Time(len(c[1]), TimezoneInfo::None),
            ("DateAndTimeType", ["Year", "OptFieldLen", "FieldOpts"]) => DataType::Custom(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), len(c[1]).map(|n| vec![n.to_string()]).unwrap_or_default()),
            (k, n) => return no(&format!("data type {k} {n:?}")),
        };
        Ok((ty, charset, None))
    }

    fn index_hints(&self, id: NodeId) -> R<Vec<TableIndexHints>> {
        let mut out = Vec::new();
        let Some(l) = self.get(id, "IndexHintList") else { return Ok(out) };
        for h in self.list(l, "IndexHint") {
            let c = self.ch(h);
            let tc = self.ch(c[0]);
            let hint_type = match self.tx(tc[0]).to_ascii_uppercase().as_str() {
                "USE" => TableIndexHintType::Use,
                "IGNORE" => TableIndexHintType::Ignore,
                _ => TableIndexHintType::Force,
            };
            let index_type = if self.tx(tc[1]).eq_ignore_ascii_case("KEY") { TableIndexType::Key } else { TableIndexType::Index };
            let for_clause = match self.ch(c[1]).get(1).map(|&w| self.tx(w).to_ascii_uppercase()) {
                Some(w) if w == "JOIN" => Some(TableIndexHintForClause::Join),
                Some(w) if w == "ORDER" => Some(TableIndexHintForClause::OrderBy),
                Some(_) => Some(TableIndexHintForClause::GroupBy),
                None => None,
            };
            let mut index_names = Vec::new();
            self.index_names(c[3], &mut index_names);
            out.push(TableIndexHints { hint_type, index_type, for_clause, index_names });
        }
        Ok(out)
    }

    fn index_names(&self, id: NodeId, out: &mut Vec<Ident>) {
        for &c in self.ch(id) {
            match self.k(c) {
                "IndexNameList" => self.index_names(c, out),
                "','" => {}
                _ => out.push(self.name_ident(c)),
            }
        }
    }

    fn table_alias(&self, id: NodeId) -> R<Option<TableAlias>> {
        match self.get(id, "TableAsName") {
            Some(a) => Ok(Some(self.table_alias_name(a)?)),
            None => Ok(None),
        }
    }

    fn table_alias_name(&self, id: NodeId) -> R<TableAlias> {
        let c = self.ch(id);
        let explicit = c.len() == 2;
        Ok(TableAlias { explicit, name: self.ident(*c.last().unwrap())?, columns: vec![], at: None })
    }

    fn table_name(&self, id: NodeId) -> R<ObjectName> {
        let mut parts = Vec::new();
        for &c in self.ch(id) {
            match self.k(c) {
                "Identifier" => parts.push(self.ident(c)?),
                "'.'" => {}
                _ => return no("*.t"),
            }
        }
        Ok(ObjectName::from(parts))
    }

    fn column_name(&self, id: NodeId) -> R<Vec<Ident>> {
        let mut parts = Vec::new();
        for &c in self.ch(id) {
            if self.k(c) == "Identifier" {
                parts.push(self.ident(c)?);
            }
        }
        Ok(parts)
    }

    // ---------------------------------------------------------------- DML

    fn insert(&self, id: NodeId) -> R<Statement> {
        let replace = self.k(id) == "ReplaceIntoStmt";
        let c = self.ch(id);
        let names = self.names(id);
        let at = |name: &str| names.iter().position(|n| *n == name).map(|i| c[i]);
        let optimizer_hints = at("TableOptimizerHintsOpt").map(|h| self.hints(h)).unwrap_or_default();
        let priority = match at("PriorityOpt").filter(|&p| !self.empty(p)) {
            Some(p) => Some(match self.tx(p).to_ascii_uppercase().as_str() {
                "LOW_PRIORITY" => MysqlInsertPriority::LowPriority,
                "DELAYED" => MysqlInsertPriority::Delayed,
                _ => MysqlInsertPriority::HighPriority,
            }),
            None => None,
        };
        let ignore = at("IgnoreOptional").is_some_and(|i| !self.empty(i));
        let into = at("IntoOpt").is_some_and(|i| !self.empty(i));
        let table = self.table_name(at("TableName").unwrap())?;
        let mut partitioned = None;
        if let Some(p) = at("PartitionNameListOpt").and_then(|p| self.get(p, "PartitionNameList")) {
            let mut v = Vec::new();
            for i in self.list(p, "Identifier") {
                v.push(Expr::Identifier(self.ident(i)?));
            }
            partitioned = Some(v);
        }
        let values = at("InsertValues").unwrap();
        let vc = self.ch(values);
        let vn = self.names(values);
        let mut columns = Vec::new();
        let mut source = None;
        let mut assignments = Vec::new();
        let mut insert_alias = None;
        let mut rest = &vn[..];
        let mut rc = vc;
        if vn[0] == "'('" {
            if let Some(l) = self.get(vc[1], "ColumnNameList") {
                for n in self.list(l, "ColumnName") {
                    columns.push(ObjectName::from(self.column_name(n)?));
                }
            }
            rest = &vn[3..];
            rc = &vc[3..];
        }
        match rest {
            ["ValueSym", "ValuesList", "InsertRowAliasOpt"] => {
                let value_keyword = self.tx(rc[0]).eq_ignore_ascii_case("VALUE");
                let mut rows = Vec::new();
                for r in self.list(rc[1], "RowValue") {
                    rows.push(self.row(r)?);
                }
                let mut q = plain(SetExpr::Values(Values { explicit_row: false, value_keyword, rows }));
                q.with = None;
                source = Some(Box::new(q));
                insert_alias = self.row_alias(rc[2])?;
            }
            ["\"SET\"", "ColumnSetValueList", "InsertRowAliasOpt"] => {
                let l = rc[1];
                let mut items = Vec::new();
                self.flatten_set_values(l, &mut items);
                for (col, val) in items {
                    assignments.push(Assignment { target: AssignmentTarget::ColumnName(ObjectName::from(self.column_name(col)?)), value: self.expr_or_default(val)? });
                }
                insert_alias = self.row_alias(rc[2])?;
            }
            [n] => {
                let q = self.query(rc[0])?;
                source = Some(Box::new(if *n == "SubSelect" { plain(SetExpr::Query(Box::new(q))) } else { q }));
            }
            other => return no(&format!("insert values {other:?}")),
        }
        let on = match at("OnDuplicateKeyUpdate").filter(|&o| !self.empty(o)) {
            Some(o) => Some(OnInsert::DuplicateKeyUpdate(self.assignments(*self.ch(o).last().unwrap())?)),
            None => None,
        };
        if at("ReturningClause").is_some_and(|r| !self.empty(r)) {
            return no("RETURNING");
        }
        Ok(Statement::Insert(Insert {
            insert_token: AttachedToken::empty(),
            optimizer_hints,
            or: None,
            ignore,
            into,
            table: TableObject::TableName(table),
            table_alias: None,
            columns,
            overwrite: false,
            source,
            assignments,
            partitioned,
            after_columns: vec![],
            has_table_keyword: false,
            on,
            returning: None,
            output: None,
            replace_into: replace,
            priority,
            insert_alias,
            settings: None,
            format_clause: None,
            multi_table_insert_type: None,
            multi_table_into_clauses: vec![],
            multi_table_when_clauses: vec![],
            multi_table_else_clause: None,
        }))
    }

    /// `/*+ … */` after a statement's first word.
    fn hints(&self, id: NodeId) -> Vec<OptimizerHint> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if self.t.node(n).leaf {
                let text = self.tx(n);
                if let Some(inner) = text.strip_prefix("/*+").and_then(|t| t.strip_suffix("*/")) {
                    out.push(OptimizerHint { prefix: String::new(), text: inner.to_string(), style: OptimizerHintStyle::MultiLine });
                }
            } else {
                stack.extend(self.ch(n).iter().rev());
            }
        }
        out
    }

    fn row_alias(&self, id: NodeId) -> R<Option<InsertAliases>> {
        let c = self.ch(id);
        if c.is_empty() {
            return Ok(None);
        }
        let row_alias = ObjectName::from(vec![self.ident(c[1])?]);
        let col_aliases = match c.get(3) {
            Some(&l) => {
                let mut v = Vec::new();
                for i in self.list(l, "Identifier") {
                    v.push(self.ident(i)?);
                }
                Some(v)
            }
            None => Some(vec![]),
        };
        Ok(Some(InsertAliases { row_alias, col_aliases }))
    }

    fn flatten_set_values(&self, id: NodeId, out: &mut Vec<(NodeId, NodeId)>) {
        let c = self.ch(id);
        if self.k(c[0]) == "ColumnSetValueList" {
            self.flatten_set_values(c[0], out);
            out.push((c[2], c[4]));
        } else {
            out.push((c[0], c[2]));
        }
    }

    fn row(&self, id: NodeId) -> R<Parens<Vec<Expr>>> {
        // RowValue: '(' ValuesOpt ')'
        let c = self.ch(id);
        let mut v = Vec::new();
        if let Some(vals) = self.get(c[1], "Values") {
            for e in self.list(vals, "ExprOrDefault") {
                v.push(self.expr_or_default(e)?);
            }
        }
        Ok(Parens { opening_token: AttachedToken::empty(), content: v, closing_token: AttachedToken::empty() })
    }

    fn expr_or_default(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id)[0];
        if self.k(c) == "\"DEFAULT\"" {
            return Ok(Expr::Identifier(Ident::new(self.tx(c))));
        }
        self.expr(c)
    }

    fn assignments(&self, id: NodeId) -> R<Vec<Assignment>> {
        let mut out = Vec::new();
        for a in self.list(id, "Assignment") {
            let c = self.ch(a);
            out.push(Assignment { target: AssignmentTarget::ColumnName(ObjectName::from(self.column_name(c[0])?)), value: self.expr_or_default(c[2])? });
        }
        Ok(out)
    }

    fn update(&self, id: NodeId) -> R<Statement> {
        let c = self.ch(id);
        if c.len() == 2 {
            // WITH … UPDATE is a query whose body is the UPDATE, as the copy reads it.
            let mut q = plain(SetExpr::Update(self.update_stmt(c[1])?));
            q.with = Some(self.with(c[0])?);
            return Ok(Statement::Query(Box::new(q)));
        }
        self.update_stmt(c[0])
    }

    fn update_stmt(&self, u: NodeId) -> R<Statement> {
        let uc = self.ch(u);
        let names = self.names(u);
        // LOW_PRIORITY and IGNORE change nothing the AST holds.
        let optimizer_hints = self.hints(uc[1]);
        let table = if names[4] == "TableRef" {
            self.table_with_joins(uc[4])?
        } else {
            comma_join(self.table_refs(uc[4])?)
        };
        let assignments = self.assignments(uc[6])?;
        let selection = self.where_clause(uc[7])?;
        let mut order_by = vec![];
        let mut limit = None;
        if names[4] == "TableRef" {
            if let Some(o) = self.get(uc[8], "OrderBy") {
                order_by = self.by_list(self.ch(o)[2])?;
            }
            if let [_, l] = self.ch(uc[9]) {
                limit = Some(self.limit_option(*l)?);
            }
        }
        if self.ch(u).last().is_some_and(|&r| self.k(r) == "ReturningClause" && !self.empty(r)) {
            return no("RETURNING");
        }
        Ok(Statement::Update(Update { update_token: AttachedToken::empty(), optimizer_hints, table, assignments, from: None, selection, returning: None, output: None, or: None, order_by, limit }))
    }

    fn delete(&self, id: NodeId) -> R<Statement> {
        let c = self.ch(id);
        if c.len() == 2 {
            let mut q = plain(SetExpr::Delete(self.delete_stmt(c[1])?));
            q.with = Some(self.with(c[0])?);
            return Ok(Statement::Query(Box::new(q)));
        }
        self.delete_stmt(c[0])
    }

    fn delete_stmt(&self, d: NodeId) -> R<Statement> {
        let dc = self.ch(d);
        let names = self.names(d);
        let optimizer_hints = self.hints(dc[1]);
        let alias_list = |n: NodeId| -> R<Vec<ObjectName>> {
            let mut v = Vec::new();
            for t in self.list(n, "TableNameOptWild") {
                let parts: Vec<Ident> = self.ch(t).iter().filter(|&&x| self.k(x) == "Identifier").map(|&x| self.ident(x)).collect::<R<_>>()?;
                v.push(ObjectName::from(parts));
            }
            Ok(v)
        };
        let (tables, from, using, selection, order_by, limit) = match &names[5..] {
            ["\"FROM\"", "TableName", "PartitionNameListOpt", "TableAsNameOptDelete", "IndexHintListOpt", "WhereClauseOptional", "OrderByOptional", "LimitClause", "ReturningClause"] => {
                if !self.empty(dc[13]) {
                    return no("DELETE RETURNING");
                }
                let mut partitions = Vec::new();
                if let Some(l) = self.get(dc[7], "PartitionNameList") {
                    for i in self.list(l, "Identifier") {
                        partitions.push(self.ident(i)?);
                    }
                }
                let relation = TableFactor::Table { name: self.table_name(dc[6])?, alias: self.table_alias(dc[8])?, args: None, with_hints: vec![], version: None, with_ordinality: false, partitions, json_path: None, sample: None, index_hints: self.index_hints(dc[9])? };
                let order_by = match self.get(dc[11], "OrderBy") {
                    Some(o) => self.by_list(self.ch(o)[2])?,
                    None => vec![],
                };
                let limit = match self.ch(dc[12]) {
                    [_, l] => Some(self.limit_option(*l)?),
                    _ => None,
                };
                (vec![], FromTable::WithFromKeyword(vec![TableWithJoins { relation, joins: vec![] }]), None, self.where_clause(dc[10])?, order_by, limit)
            }
            ["TableAliasRefList", "\"FROM\"", "TableRefs", "WhereClauseOptional"] => (alias_list(dc[5])?, FromTable::WithFromKeyword(self.table_refs(dc[7])?), None, self.where_clause(dc[8])?, vec![], None),
            ["\"FROM\"", "TableAliasRefList", "\"USING\"", "TableRefs", "WhereClauseOptional"] => {
                let from = alias_list(dc[6])?.into_iter().map(|name| TableWithJoins { relation: TableFactor::Table { name, alias: None, args: None, with_hints: vec![], version: None, with_ordinality: false, partitions: vec![], json_path: None, sample: None, index_hints: vec![] }, joins: vec![] }).collect();
                (vec![], FromTable::WithFromKeyword(from), Some(self.table_refs(dc[8])?), self.where_clause(dc[9])?, vec![], None)
            }
            other => return no(&format!("delete {other:?}")),
        };
        Ok(Statement::Delete(Delete { delete_token: AttachedToken::empty(), optimizer_hints, tables, from, using, selection, returning: None, output: None, order_by, limit }))
    }

    // ---------------------------------------------------------------- expressions

    /// Any expression-shaped node.
    fn expr_any(&self, id: NodeId) -> R<Expr> {
        match self.k(id) {
            "Expression" | "BoolPri" | "PredicateExpr" | "BitExpr" | "SimpleExpr" => self.expr(id),
            _ if self.t.node(id).leaf || self.ch(id).len() == 1 => self.leafish(id),
            other => no(&format!("expr_any {other}")),
        }
    }

    /// Chains of one child (`LimitOption`, `LengthNum`, `NUM`, `Literal`, `Variable`…).
    fn leafish(&self, id: NodeId) -> R<Expr> {
        if !self.t.node(id).leaf {
            let c = self.ch(id);
            return match self.k(id) {
                "Literal" | "NumLiteral" => self.literal(id),
                "SimpleIdent" => self.simple_ident(id),
                "Variable" | "SystemVariable" | "UserVariable" | "SelectIntoVar" => self.leafish(c[0]),
                "SelectIntoVars" => no("into vars"),
                _ if c.len() == 1 => self.leafish(c[0]),
                other => no(&format!("leafish {other}")),
            };
        }
        let text = self.tx(id);
        match self.k(id) {
            "\"integer literal\"" | "\"decimal literal\"" | "\"floating-point literal\"" => Ok(self.number(id)),
            "\"?\"" => Ok(Expr::Value(ValueWithSpan { value: Value::Placeholder(text.to_string()), span: self.span(id) })),
            "\"identifier with single leading at\"" | "\"identifier with double leading at\"" => Ok(self.variable(id)),
            "\"identifier\"" => Ok(Expr::Identifier(self.leaf_ident(id))),
            other => no(&format!("leaf {other} {text:?}")),
        }
    }

    fn variable(&self, id: NodeId) -> Expr {
        let text = self.tx(id);
        // `@@session.x` and `@@global.x` are two parts, as the copy reads them.
        if let Some(rest) = text.strip_prefix("@@") {
            if rest.contains('.') {
                let mut parts = rest.split('.');
                let mut v = vec![Ident::new(format!("@@{}", parts.next().unwrap()))];
                v.extend(parts.map(Ident::new));
                return Expr::CompoundIdentifier(v);
            }
        }
        Expr::Identifier(Ident::new(text))
    }

    fn number(&self, id: NodeId) -> Expr {
        Expr::Value(ValueWithSpan { value: Value::Number(self.tx(id).to_string(), false), span: self.span(id) })
    }

    pub(crate) fn expr(&self, id: NodeId) -> R<Expr> {
        self.deeper(|| self.expr_at(id))
    }

    /// An operand of an operator, which spends no depth: a chain of operators is a loop's worth of
    /// work in the parser, and a statement's operators are bounded together instead.
    fn operand(&self, id: NodeId) -> R<Expr> {
        self.expr_at(id)
    }

    fn expr_at(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let names = self.names(id);
        match (self.k(id), names.as_slice()) {
            (_, [_]) if matches!(self.k(id), "Expression" | "BoolPri" | "PredicateExpr" | "BitExpr") => self.operand(c[0]),
            ("Expression", ["Expression", op, "Expression"]) => {
                let op = match *op {
                    "logOr" => if self.tx(c[1]) == "||" { BinaryOperator::Or } else { BinaryOperator::Or },
                    "logAnd" => BinaryOperator::And,
                    "\"XOR\"" => BinaryOperator::Xor,
                    other => return no(&format!("logical {other}")),
                };
                Ok(Expr::BinaryOp { left: Box::new(self.operand(c[0])?), op, right: Box::new(self.operand(c[2])?) })
            }
            ("Expression", ["\"NOT\"", "Expression"]) => match self.operand(c[1])? {
                // `NOT EXISTS (…)` is one negated EXISTS, as the copy reads it.
                Expr::Exists { subquery, negated: false } if self.is_exists(c[1]) => Ok(Expr::Exists { subquery, negated: true }),
                e => Ok(Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(e) }),
            },
            ("Expression", ["BoolPri", "IsOrNotOp", what]) => {
                let negated = self.ch(c[1]).len() == 2;
                let e = Box::new(self.operand(c[0])?);
                Ok(match (*what, negated) {
                    ("trueKwd" | "\"TRUE\"", false) => Expr::IsTrue(e),
                    ("trueKwd" | "\"TRUE\"", true) => Expr::IsNotTrue(e),
                    ("falseKwd" | "\"FALSE\"", false) => Expr::IsFalse(e),
                    ("falseKwd" | "\"FALSE\"", true) => Expr::IsNotFalse(e),
                    (_, false) => Expr::IsUnknown(e),
                    (_, true) => Expr::IsNotUnknown(e),
                })
            }
            ("Expression", [_, "\":=\"", "Expression"]) => Ok(Expr::BinaryOp { left: Box::new(self.variable(c[0])), op: BinaryOperator::Assignment, right: Box::new(self.operand(c[2])?) }),
            ("BoolPri", ["BoolPri", "CompareOp", _, "\":=\"", "PredicateExpr"]) => Ok(Expr::BinaryOp { left: Box::new(self.operand(c[0])?), op: self.compare_op(c[1]), right: Box::new(Expr::BinaryOp { left: Box::new(self.variable(c[2])), op: BinaryOperator::Assignment, right: Box::new(self.operand(c[4])?) }) }),
            ("BoolPri", ["BoolPri", "IsOrNotOp", _]) => {
                let e = Box::new(self.operand(c[0])?);
                Ok(if self.ch(c[1]).len() == 2 { Expr::IsNotNull(e) } else { Expr::IsNull(e) })
            }
            ("BoolPri", ["BoolPri", "CompareOp", "PredicateExpr"]) => Ok(Expr::BinaryOp { left: Box::new(self.operand(c[0])?), op: self.compare_op(c[1]), right: Box::new(self.operand(c[2])?) }),
            ("BoolPri", ["BoolPri", "CompareOp", "AnyOrAll", "SubSelect"]) => {
                let left = Box::new(self.operand(c[0])?);
                let compare_op = self.compare_op(c[1]);
                let right = Box::new(Expr::Subquery(Box::new(self.subquery(c[3])?)));
                let w = self.tx(c[2]).to_ascii_uppercase();
                Ok(if w == "ALL" { Expr::AllOp { left, compare_op, right } } else { Expr::AnyOp { left, compare_op, right, is_some: w == "SOME" } })
            }
            ("PredicateExpr", ["BitExpr", "InOrNotOp", "'('", "ExpressionList", "')'"]) => {
                let mut list = Vec::new();
                for e in self.list(c[3], "Expression") {
                    list.push(self.operand(e)?);
                }
                Ok(Expr::InList { expr: Box::new(self.operand(c[0])?), list, negated: self.ch(c[1]).len() == 2 })
            }
            ("PredicateExpr", ["BitExpr", "InOrNotOp", "SubSelect"]) => Ok(Expr::InSubquery { expr: Box::new(self.operand(c[0])?), subquery: Box::new(self.subquery(c[2])?), negated: self.ch(c[1]).len() == 2 }),
            ("PredicateExpr", ["BitExpr", "BetweenOrNotOp", "BitExpr", "\"AND\"", "PredicateExpr"]) => Ok(Expr::Between { expr: Box::new(self.operand(c[0])?), negated: self.ch(c[1]).len() == 2, low: Box::new(self.operand(c[2])?), high: Box::new(self.operand(c[4])?) }),
            ("PredicateExpr", ["BitExpr", "LikeOrNotOp", "SimpleExpr", "LikeOrIlikeEscapeOpt"]) => {
                let escape_char = match self.ch(c[3]) {
                    [_, s] => Some(Box::new(self.operand(*s)?)),
                    _ => None,
                };
                Ok(Expr::Like { negated: self.ch(c[1]).len() == 2, any: false, expr: Box::new(self.operand(c[0])?), pattern: Box::new(self.operand(c[2])?), escape_char })
            }
            ("PredicateExpr", ["BitExpr", "RegexpOrNotOp", "SimpleExpr"]) => {
                let rc = self.ch(c[1]);
                let sym = *rc.last().unwrap();
                Ok(Expr::RLike { negated: rc.len() == 2, expr: Box::new(self.operand(c[0])?), pattern: Box::new(self.operand(c[2])?), regexp: self.tx(sym).eq_ignore_ascii_case("REGEXP") })
            }
            ("PredicateExpr", ["BitExpr", "\"MEMBER OF\"", "'('", "SimpleExpr", "')'"]) => Ok(Expr::MemberOf(MemberOf { value: Box::new(self.operand(c[0])?), array: Box::new(self.operand(c[3])?) })),
            ("BitExpr", ["BitExpr", op, "BitExpr"]) => {
                let op = match *op {
                    "'|'" => BinaryOperator::BitwiseOr,
                    "'&'" => BinaryOperator::BitwiseAnd,
                    "\"<<\"" => BinaryOperator::PGBitwiseShiftLeft,
                    "\">>\"" => BinaryOperator::PGBitwiseShiftRight,
                    "'+'" => BinaryOperator::Plus,
                    "'-'" => BinaryOperator::Minus,
                    "'*'" => BinaryOperator::Multiply,
                    "'/'" => BinaryOperator::Divide,
                    "'%'" | "\"MOD\"" => BinaryOperator::Modulo,
                    "\"DIV\"" => BinaryOperator::MyIntegerDivide,
                    "'^'" => BinaryOperator::BitwiseXor,
                    other => return no(&format!("bit op {other}")),
                };
                Ok(Expr::BinaryOp { left: Box::new(self.operand(c[0])?), op, right: Box::new(self.operand(c[2])?) })
            }
            ("BitExpr", ["BitExpr", sign, "\"INTERVAL\"", "Expression", "TimeUnit"]) => {
                let op = if *sign == "'+'" { BinaryOperator::Plus } else { BinaryOperator::Minus };
                Ok(Expr::BinaryOp { left: Box::new(self.operand(c[0])?), op, right: Box::new(self.interval(c[3], c[4])?) })
            }
            ("BitExpr", ["\"INTERVAL\"", "Expression", "TimeUnit", "'+'", "BitExpr"]) => Ok(Expr::BinaryOp { left: Box::new(self.interval(c[1], c[2])?), op: BinaryOperator::Plus, right: Box::new(self.operand(c[4])?) }),
            ("SimpleExpr", _) => self.simple(id),
            (k, n) => no(&format!("expr {k} {n:?}")),
        }
    }

    /// Whether an expression is an EXISTS with nothing around it.
    fn is_exists(&self, id: NodeId) -> bool {
        let mut n = id;
        loop {
            let c = self.ch(n);
            if c.len() == 1 && matches!(self.k(n), "Expression" | "BoolPri" | "PredicateExpr" | "BitExpr") {
                n = c[0];
                continue;
            }
            return self.k(n) == "SimpleExpr" && c.first().is_some_and(|&f| self.k(f) == "\"EXISTS\"");
        }
    }

    fn compare_op(&self, id: NodeId) -> BinaryOperator {
        match self.tx(id) {
            ">=" => BinaryOperator::GtEq,
            ">" => BinaryOperator::Gt,
            "<=" => BinaryOperator::LtEq,
            "<" => BinaryOperator::Lt,
            "!=" => BinaryOperator::NotEq,
            "<>" => BinaryOperator::NotEq,
            "<=>" => BinaryOperator::Spaceship,
            _ => BinaryOperator::Eq,
        }
    }

    fn interval(&self, value: NodeId, unit: NodeId) -> R<Expr> {
        Ok(Expr::Interval(Interval { value: Box::new(self.expr(value)?), leading_field: Some(self.time_unit(unit)?), leading_precision: None, last_field: None, fractional_seconds_precision: None }))
    }

    fn time_unit(&self, id: NodeId) -> R<DateTimeField> {
        Ok(match self.tx(id).to_ascii_uppercase().as_str() {
            "MICROSECOND" | "SQL_TSI_MICROSECOND" => DateTimeField::Microsecond,
            "SECOND" | "SQL_TSI_SECOND" => DateTimeField::Second,
            "MINUTE" | "SQL_TSI_MINUTE" => DateTimeField::Minute,
            "HOUR" | "SQL_TSI_HOUR" => DateTimeField::Hour,
            "DAY" | "SQL_TSI_DAY" => DateTimeField::Day,
            "WEEK" | "SQL_TSI_WEEK" => DateTimeField::Week(None),
            "MONTH" | "SQL_TSI_MONTH" => DateTimeField::Month,
            "QUARTER" | "SQL_TSI_QUARTER" => DateTimeField::Quarter,
            "YEAR" | "SQL_TSI_YEAR" => DateTimeField::Year,
            // A compound unit as its smallest part, as the engine reads it.
            "SECOND_MICROSECOND" | "MINUTE_MICROSECOND" | "HOUR_MICROSECOND" | "DAY_MICROSECOND" => DateTimeField::Microsecond,
            "MINUTE_SECOND" | "HOUR_SECOND" | "DAY_SECOND" => DateTimeField::Second,
            "HOUR_MINUTE" | "DAY_MINUTE" => DateTimeField::Minute,
            "DAY_HOUR" => DateTimeField::Hour,
            "YEAR_MONTH" => DateTimeField::Month,
            other => return no(&format!("time unit {other}")),
        })
    }

    fn simple_ident(&self, id: NodeId) -> R<Expr> {
        let parts = self.column_name(id)?;
        Ok(if parts.len() == 1 { Expr::Identifier(parts.into_iter().next().unwrap()) } else { Expr::CompoundIdentifier(parts) })
    }

    fn simple(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let names = self.names(id);
        match names.as_slice() {
            ["SimpleIdent"] => self.simple_ident(c[0]),
            ["Literal"] => self.literal(c[0]),
            ["\"?\""] => self.leafish(c[0]),
            ["Variable"] => self.leafish(c[0]),
            ["FunctionCallGeneric"] => self.call_generic(c[0]),
            ["FunctionCallKeyword"] | ["FunctionCallNonKeyword"] => self.call_builtin(c[0]),
            ["SumExpr"] => self.sum(c[0]),
            ["WindowFuncCall"] => self.window_call(c[0]),
            ["\"BINARY\"", "SimpleExpr"] if self.names(c[1]).first() == Some(&"'('") && self.names(c[1]).len() == 3 => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![self.expr(self.ch(c[1])[1])?])),
            ["\"BINARY\"", "SimpleExpr"] => Ok(Expr::Cast { kind: CastKind::Cast, expr: Box::new(self.expr(c[1])?), data_type: DataType::Binary(None), format: None, charset: None }),
            ["builtinCast", "'('", "Expression", "\"AS\"", "CastType", "ArrayKwdOpt", "')'"] => {
                if !self.empty(c[5]) {
                    return no("CAST AS … ARRAY");
                }
                let (data_type, charset) = self.cast_type(c[4])?;
                Ok(Expr::Cast { kind: CastKind::Cast, expr: Box::new(self.expr(c[2])?), data_type, format: None, charset })
            }
            ["builtinCast", "'('", "Expression", "\"AT\"", "\"TIME\"", "\"ZONE\"", rest @ ..] => {
                if rest[0] == "\"INTERVAL\"" {
                    return no("AT TIME ZONE INTERVAL");
                }
                let (data_type, charset) = self.cast_type(c[8])?;
                let expr = Expr::AtTimeZone { timestamp: Box::new(self.expr(c[2])?), time_zone: Box::new(self.string_literal(c[6])) };
                Ok(Expr::Cast { kind: CastKind::Cast, expr: Box::new(expr), data_type, format: None, charset })
            }
            ["\"CONVERT\"", "'('", "Expression", "','", "CastType", "')'"] => {
                let (data_type, charset) = self.cast_type(c[4])?;
                Ok(Expr::Convert { is_try: false, expr: Box::new(self.expr(c[2])?), data_type: Some(data_type), charset, target_before_value: false, styles: vec![] })
            }
            ["\"CONVERT\"", "'('", "Expression", "\"USING\"", "CharsetName", "')'"] => Ok(Expr::Convert { is_try: false, expr: Box::new(self.expr(c[2])?), data_type: None, charset: Some(ObjectName::from(vec![self.name_ident(c[4])])), target_before_value: false, styles: vec![] }),
            ["\"DEFAULT\"", "'('", "SimpleIdent", "')'"] | ["\"VALUES\"", "'('", "SimpleIdent", "')'"] => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![self.simple_ident(c[2])?])),
            ["\"ROW\"", "'('", "ExpressionList", "','", "Expression", "')'"] => {
                let mut v = Vec::new();
                for e in self.list(c[2], "Expression") {
                    v.push(self.expr(e)?);
                }
                v.push(self.expr(c[4])?);
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), v))
            }
            ["SimpleIdent", arrow, "\"string literal\""] if *arrow == "\"->\"" || *arrow == "\"->>\"" => {
                let op = if *arrow == "\"->\"" { BinaryOperator::Arrow } else { BinaryOperator::LongArrow };
                Ok(Expr::BinaryOp { left: Box::new(self.simple_ident(c[0])?), op, right: Box::new(self.string_literal(c[2])) })
            }
            ["\"MATCH\"", "'('", "ColumnNameList", "')'", "\"AGAINST\"", "'('", "BitExpr", "FulltextSearchModifierOpt", "')'"] => self.match_against(c[2], c[6], c[7]),
            ["\"MATCH\"", "ColumnNameList", "\"AGAINST\"", "'('", "BitExpr", "FulltextSearchModifierOpt", "')'"] => self.match_against(c[1], c[4], c[5]),
            ["SimpleExpr", "\"COLLATE\"", "CollationName"] => Ok(Expr::Collate { expr: Box::new(self.expr(c[0])?), collation: ObjectName::from(vec![self.name_ident(c[2])]) }),
            ["'!'", "SimpleExpr"] => Ok(Expr::UnaryOp { op: UnaryOperator::BangNot, expr: Box::new(self.expr(c[1])?) }),
            ["not2", "SimpleExpr"] => Ok(Expr::UnaryOp { op: UnaryOperator::BangNot, expr: Box::new(self.expr(c[1])?) }),
            ["'~'", "SimpleExpr"] => Ok(Expr::UnaryOp { op: UnaryOperator::BitwiseNot, expr: Box::new(self.expr(c[1])?) }),
            ["'-'", "SimpleExpr"] => Ok(Expr::UnaryOp { op: UnaryOperator::Minus, expr: Box::new(self.expr(c[1])?) }),
            ["'+'", "SimpleExpr"] => Ok(Expr::UnaryOp { op: UnaryOperator::Plus, expr: Box::new(self.expr(c[1])?) }),
            ["SimpleExpr", "\"||\"", "SimpleExpr"] => Ok(Expr::BinaryOp { left: Box::new(self.operand(c[0])?), op: BinaryOperator::StringConcat, right: Box::new(self.operand(c[2])?) }),
            ["SubSelect"] => Ok(Expr::Subquery(Box::new(self.subquery(c[0])?))),
            // A parenthesized operand spends two levels, as a subquery does.
            ["'('", "Expression", "')'"] => Ok(Expr::Nested(Box::new(self.deeper(|| self.expr(c[1]))?))),
            ["'('", "ExpressionList", "','", "Expression", "')'"] => {
                let mut v = Vec::new();
                for e in self.list(c[1], "Expression") {
                    v.push(self.expr(e)?);
                }
                v.push(self.expr(c[3])?);
                Ok(Expr::Tuple(v))
            }
            ["'{'", "Identifier", "Expression", "'}'"] => {
                let kind = self.tx(c[1]).to_ascii_lowercase();
                let e = self.expr(c[2])?;
                match (kind.as_str(), e) {
                    ("fn", Expr::Function(f)) => Ok(Expr::Function(Function { uses_odbc_syntax: true, ..f })),
                    (k @ ("d" | "t" | "ts"), Expr::Value(value)) => {
                        let data_type = match k {
                            "d" => DataType::Date,
                            "t" => DataType::Time(None, TimezoneInfo::None),
                            _ => DataType::Timestamp(None, TimezoneInfo::None),
                        };
                        Ok(Expr::TypedString(TypedString { data_type, value, uses_odbc_syntax: true }))
                    }
                    (_, e) => Ok(e),
                }
            }
            ["\"EXISTS\"", "SubSelect"] => Ok(Expr::Exists { subquery: Box::new(self.subquery(c[1])?), negated: false }),
            ["\"CASE\"", "ExpressionOpt", "WhenClauseList", "ElseOpt", "\"END\""] => {
                let operand = match self.ch(c[1]).first() {
                    Some(&e) => Some(Box::new(self.expr(e)?)),
                    None => None,
                };
                let mut conditions = Vec::new();
                for w in self.list(c[2], "WhenClause") {
                    let wc = self.ch(w);
                    conditions.push(CaseWhen { condition: self.expr(wc[1])?, result: self.expr(wc[3])? });
                }
                let else_result = match self.ch(c[3]) {
                    [_, e] => Some(Box::new(self.expr(*e)?)),
                    _ => None,
                };
                Ok(Expr::Case { case_token: AttachedToken::empty(), end_token: AttachedToken::empty(), operand, conditions, else_result })
            }
            other => no(&format!("simple {other:?}")),
        }
    }

    fn sum(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let names = self.names(id);
        let name = ObjectName::from(vec![Ident::new(self.tx(c[0]))]);
        let mut args = Vec::new();
        let mut duplicate_treatment = None;
        let mut over = None;
        let mut clauses = Vec::new();
        let mut i = 2;
        while i < c.len() {
            match names[i] {
                "')'" => {}
                "'*'" => args.push(FunctionArg::Unnamed(FunctionArgExpr::Wildcard)),
                "\"ALL\"" => duplicate_treatment = Some(DuplicateTreatment::All),
                "DistinctKwd" => duplicate_treatment = Some(DuplicateTreatment::Distinct),
                "BuggyDefaultFalseDistinctOpt" => {
                    if !self.empty(c[i]) {
                        let words = self.tx(c[i]).to_ascii_uppercase();
                        if words.starts_with("DISTINCT") {
                            duplicate_treatment = Some(DuplicateTreatment::Distinct);
                        } else if words == "ALL" {
                            duplicate_treatment = Some(DuplicateTreatment::All);
                        }
                    }
                }
                "Expression" => args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(self.expr(c[i])?))),
                "ExpressionList" => {
                    for e in self.list(c[i], "Expression") {
                        args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(self.expr(e)?)));
                    }
                }
                "OrderByOptional" => {
                    if let Some(o) = self.get(c[i], "OrderBy") {
                        clauses.push(FunctionArgumentClause::OrderBy(self.by_list(self.ch(o)[2])?));
                    }
                }
                "OptGConcatSeparator" => {
                    if let [_, s] = self.ch(c[i]) {
                        clauses.push(FunctionArgumentClause::Separator(match self.string_value(*s) {
                            Expr::Value(v) => v,
                            _ => unreachable!(),
                        }));
                    }
                }
                "OptWindowingClause" => {
                    if let Some(w) = self.get(c[i], "WindowingClause") {
                        over = Some(self.windowing(w)?);
                    }
                }
                "','" => {}
                other => return no(&format!("sum arg {other}")),
            }
            i += 1;
        }
        let _ = &mut over;
        Ok(Expr::Function(Function { name, uses_odbc_syntax: false, parameters: FunctionArguments::None, args: FunctionArguments::List(FunctionArgumentList { duplicate_treatment, args, clauses }), within_group: vec![], filter: None, null_treatment: None, over }))
    }

    fn call_generic(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let names = self.names(id);
        let (name, list) = match names.as_slice() {
            ["\"identifier\"", "'('", "ExpressionListOpt", "')'"] => (ObjectName::from(vec![self.leaf_ident(c[0])]), c[2]),
            ["Identifier", "'.'", "Identifier", "'('", "ExpressionListOpt", "')'"] => (ObjectName::from(vec![self.ident(c[0])?, self.ident(c[2])?]), c[4]),
            ["\"identifier\"", "'('", "ExpressionList", "\"RETURNING\"", "CastType", "JsonValueOn", "')'"] => {
                if !self.empty(c[5]) {
                    return no("JSON_VALUE ON EMPTY");
                }
                let (data_type, _) = self.cast_type(c[4])?;
                let args = self.list(c[2], "Expression").into_iter().map(|e| Ok(FunctionArg::Unnamed(FunctionArgExpr::Expr(self.expr(e)?)))).collect::<R<Vec<_>>>()?;
                let clauses = vec![FunctionArgumentClause::JsonReturningClause(JsonReturningClause { data_type })];
                return Ok(Expr::Function(Function { args: FunctionArguments::List(FunctionArgumentList { duplicate_treatment: None, args, clauses }), ..func_parts(ObjectName::from(vec![self.leaf_ident(c[0])])) }));
            }
            other => return no(&format!("generic call {other:?}")),
        };
        let mut args = self.expr_list_opt(list)?;
        if let [ObjectNamePart::Identifier(n)] = name.0.as_slice() {
            let upper = n.value.to_ascii_uppercase();
            if (upper == "FLOOR" || upper == "CEIL") && (args.len() == 1 || args.len() == 2) {
                let field = if args.len() == 2 {
                    match args.pop().unwrap() {
                        Expr::Value(v) => CeilFloorKind::Scale(v),
                        other => {
                            args.push(other);
                            return Ok(func(name, args));
                        }
                    }
                } else {
                    CeilFloorKind::DateTimeField(DateTimeField::NoDateTime)
                };
                let expr = Box::new(args.pop().unwrap());
                return Ok(if upper == "FLOOR" { Expr::Floor { expr, field } } else { Expr::Ceil { expr, field } });
            }
        }
        Ok(func(name, args))
    }

    fn expr_list_opt(&self, id: NodeId) -> R<Vec<Expr>> {
        let mut out = Vec::new();
        if let Some(l) = self.get(id, "ExpressionList") {
            for e in self.list(l, "Expression") {
                out.push(self.expr(e)?);
            }
        }
        Ok(out)
    }

    fn call_builtin(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let names = self.names(id);
        match names.as_slice() {
            ["FunctionNameConflict", "'('", "ExpressionListOpt", "')'"] | ["builtinUser", "'('", "ExpressionListOpt", "')'"] | ["\"INSERT\"", "'('", "ExpressionListOpt", "')'"] | ["\"PASSWORD\"", "'('", "ExpressionListOpt", "')'"] | ["\"COMPRESS\"", "'('", "ExpressionListOpt", "')'"] => {
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), self.expr_list_opt(c[2])?))
            }
            ["\"GROUPING\"", "'('", "ExpressionList", "')'"] => {
                let mut args = Vec::new();
                for e in self.list(c[2], "Expression") {
                    args.push(self.expr(e)?);
                }
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            ["\"WEIGHT_STRING\"", "'('", "Expression", "','", "Expression", "','", "Expression", "','", "Expression", "')'"] => {
                let args = c.iter().filter(|&&e| self.k(e) == "Expression").map(|&e| self.expr(e)).collect::<R<Vec<_>>>()?;
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            ["\"MOD\"", "'('", "Expression", "','", "Expression", "')'"] => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![self.expr(c[2])?, self.expr(c[4])?])),
            ["builtinCurDate", "'('", "')'"] | ["builtinCurTime", "'('", "FuncDatetimePrecListOpt", "')'"] | ["builtinSysDate", "'('", "FuncDatetimePrecListOpt", "')'"] => {
                let mut args = Vec::new();
                if names.len() == 4 && !self.empty(c[2]) {
                    args.push(self.expr_any(self.ch(c[2])[0])?);
                }
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            ["FunctionNameOptionalBraces", "OptionalBraces"] => {
                let name = Ident::new(self.tx(c[0]));
                if self.empty(c[1]) {
                    // Without parentheses, CURRENT_DATE is a call with no argument list, and the others a name.
                    if self.tx(c[0]).eq_ignore_ascii_case("CURRENT_DATE") {
                        return Ok(Expr::Function(Function { args: FunctionArguments::None, ..func_parts(ObjectName::from(vec![name])) }));
                    }
                    return Ok(Expr::Identifier(name));
                }
                Ok(func(ObjectName::from(vec![name]), vec![]))
            }
            ["FunctionNameDatetimePrecision", "FuncDatetimePrec"] => {
                let name = ObjectName::from(vec![Ident::new(self.tx(c[0]))]);
                match self.ch(c[1]) {
                    // UTC_TIME and UTC_TIMESTAMP without parentheses are names, as the copy reads them.
                    [] if self.tx(c[0]).to_ascii_uppercase().starts_with("UTC_") => Ok(Expr::Identifier(Ident::new(self.tx(c[0])))),
                    [] => Ok(Expr::Function(Function { args: FunctionArguments::None, ..func_parts(name) })),
                    [_, _] => Ok(func(name, vec![])),
                    [_, n, _] => Ok(func(name, vec![self.number(*n)])),
                    _ => no("datetime precision"),
                }
            }
            ["FunctionNameDateArith" | "FunctionNameDateArithMultiForms", "'('", "Expression", "','", "\"INTERVAL\"", "Expression", "TimeUnit", "')'"] => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![self.expr(c[2])?, self.interval(c[5], c[6])?])),
            ["FunctionNameDateArithMultiForms", "'('", "Expression", "','", "Expression", "')'"] => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![self.expr(c[2])?, self.expr(c[4])?])),
            ["builtinExtract", "'('", "TimeUnit", "\"FROM\"", "Expression", "')'"] => Ok(Expr::Extract { field: self.time_unit(c[2])?, syntax: ExtractSyntax::From, expr: Box::new(self.expr(c[4])?) }),
            ["\"GET_FORMAT\"", "'('", "GetFormatSelector", "','", "Expression", "')'"] => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![Expr::Identifier(Ident::new(self.tx(c[2]))), self.expr(c[4])?])),
            ["builtinPosition", "'('", "BitExpr", "\"IN\"", "Expression", "')'"] => Ok(Expr::Position { expr: Box::new(self.expr(c[2])?), r#in: Box::new(self.expr(c[4])?) }),
            ["builtinSubstring", "'('", ..] if self.tx(c[0]).eq_ignore_ascii_case("MID") => {
                let args = c.iter().filter(|&&e| self.k(e) == "Expression").map(|&e| self.expr(e)).collect::<R<Vec<_>>>()?;
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            ["builtinSubstring", "'('", rest @ ..] => {
                let shorthand = self.tx(c[0]).eq_ignore_ascii_case("SUBSTR");
                let special = rest.get(1) == Some(&"','");
                let from = Some(Box::new(self.expr(c[4])?));
                let for_ = if c.len() == 8 { Some(Box::new(self.expr(c[6])?)) } else { None };
                Ok(Expr::Substring { expr: Box::new(self.expr(c[2])?), substring_from: from, substring_for: for_, special, shorthand })
            }
            ["\"TIMESTAMPADD\"" | "\"TIMESTAMPDIFF\"", "'('", "TimestampUnit", "','", "Expression", "','", "Expression", "')'"] => Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![Expr::Identifier(Ident::new(self.tx(c[2]))), self.expr(c[4])?, self.expr(c[6])?])),
            ["builtinTrim", "'('", "Expression", "')'"] => Ok(Expr::Trim { trim_where: None, trim_what: None, expr: Box::new(self.expr(c[2])?), trim_characters: None }),
            ["builtinTrim", "'('", "Expression", "\"FROM\"", "Expression", "')'"] => Ok(Expr::Trim { trim_where: None, trim_what: Some(Box::new(self.expr(c[2])?)), expr: Box::new(self.expr(c[4])?), trim_characters: None }),
            ["builtinTrim", "'('", "TrimDirection", "\"FROM\"", "Expression", "')'"] => Ok(Expr::Trim { trim_where: Some(self.trim_where(c[2])), trim_what: None, expr: Box::new(self.expr(c[4])?), trim_characters: None }),
            ["builtinTrim", "'('", "TrimDirection", "Expression", "\"FROM\"", "Expression", "')'"] => Ok(Expr::Trim { trim_where: Some(self.trim_where(c[2])), trim_what: Some(Box::new(self.expr(c[3])?)), expr: Box::new(self.expr(c[5])?), trim_characters: None }),
            ["\"WEIGHT_STRING\"", "'('", "Expression", "')'"] | ["builtinTranslate", ..] => {
                let mut args = Vec::new();
                for &e in c.iter().filter(|&&e| self.k(e) == "Expression") {
                    args.push(self.expr(e)?);
                }
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            ["\"CHAR\"", "'('", "ExpressionList", "')'"] => {
                let mut args = Vec::new();
                for e in self.list(c[2], "Expression") {
                    args.push(self.expr(e)?);
                }
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            ["\"CHAR\"", "'('", "ExpressionList", "\"USING\"", "CharsetName", "')'"] => {
                let mut args = Vec::new();
                for e in self.list(c[2], "Expression") {
                    args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(self.expr(e)?)));
                }
                let clauses = vec![FunctionArgumentClause::Using(ObjectName::from(vec![self.name_ident(c[4])]))];
                Ok(Expr::Function(Function { args: FunctionArguments::List(FunctionArgumentList { duplicate_treatment: None, args, clauses }), ..func_parts(ObjectName::from(vec![Ident::new(self.tx(c[0]))])) }))
            }
            ["\"DATE\"" | "\"TIME\"" | "\"TIMESTAMP\"", "\"string literal\""] => {
                let data_type = match self.tx(c[0]).to_ascii_uppercase().as_str() {
                    "DATE" => DataType::Date,
                    "TIME" => DataType::Time(None, TimezoneInfo::None),
                    _ => DataType::Timestamp(None, TimezoneInfo::None),
                };
                let Expr::Value(value) = self.string_literal(c[1]) else { unreachable!() };
                Ok(Expr::TypedString(TypedString { data_type, value, uses_odbc_syntax: false }))
            }
            ["\"INTERVAL\"", "'('", ..] => {
                let mut args = Vec::new();
                for &e in &c[2..] {
                    match self.k(e) {
                        "Expression" => args.push(self.expr(e)?),
                        "ExpressionList" => {
                            for x in self.list(e, "Expression") {
                                args.push(self.expr(x)?);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(func(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), args))
            }
            other => no(&format!("builtin {other:?}")),
        }
    }

    fn trim_where(&self, id: NodeId) -> TrimWhereField {
        match self.tx(id).to_ascii_uppercase().as_str() {
            "BOTH" => TrimWhereField::Both,
            "LEADING" => TrimWhereField::Leading,
            _ => TrimWhereField::Trailing,
        }
    }

    fn match_against(&self, columns: NodeId, value: NodeId, modifier: NodeId) -> R<Expr> {
        let mut cols = Vec::new();
        for n in self.list(columns, "ColumnName") {
            cols.push(ObjectName::from(self.column_name(n)?));
        }
        let Expr::Value(match_value) = self.expr(value)? else { return no("AGAINST a non-literal") };
        let words: Vec<String> = self.ch(modifier).iter().map(|&w| self.tx(w).to_ascii_uppercase()).collect();
        let opt_search_modifier = match words.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
            [] => None,
            ["IN", "NATURAL", "LANGUAGE", "MODE"] => Some(SearchModifier::InNaturalLanguageMode),
            ["IN", "NATURAL", "LANGUAGE", "MODE", ..] => Some(SearchModifier::InNaturalLanguageModeWithQueryExpansion),
            ["IN", "BOOLEAN", "MODE"] => Some(SearchModifier::InBooleanMode),
            _ => Some(SearchModifier::WithQueryExpansion),
        };
        Ok(Expr::MatchAgainst { columns: cols, match_value, opt_search_modifier })
    }

    /// A CastType as sqlparser's data type, and the character set it names.
    fn cast_type(&self, id: NodeId) -> R<(DataType, Option<ObjectName>)> {
        let c = self.ch(id);
        let names = self.names(id);
        let len = |n: NodeId| -> Option<u64> { self.get(n, "FieldLen").map(|f| self.tx(self.ch(f)[1]).parse().unwrap_or(0)) };
        let field_len = |n: NodeId| -> Option<u64> {
            if self.k(n) == "FieldLen" {
                return Some(self.tx(self.ch(n)[1]).parse().unwrap_or(0));
            }
            len(n)
        };
        let word = self.tx(c[0]).to_ascii_uppercase();
        let ty = match names.as_slice() {
            ["\"BINARY\"", "OptFieldLen"] => DataType::Binary(field_len(c[1])),
            ["Char", "OptFieldLen", "OptCharsetWithOptBinary"] => {
                let l = field_len(c[1]).map(|length| CharacterLength::IntegerLength { length, unit: None });
                // ASCII and UNICODE stand for latin1 and ucs2, BINARY for binary.
                let charset = match self.ch(c[2]).first() {
                    None => None,
                    Some(&b) if self.k(b) == "OptBinary" => self.opt_binary(b)?,
                    Some(&w) => match self.tx(w).to_ascii_uppercase().as_str() {
                        "ASCII" => Some(ObjectName::from(vec![Ident::new("latin1")])),
                        "UNICODE" => Some(ObjectName::from(vec![Ident::new("ucs2")])),
                        _ => return no("CHAR BYTE"),
                    },
                };
                return Ok((DataType::Char(l), charset));
            }
            ["NChar", "OptFieldLen"] => DataType::Custom(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), field_len(c[1]).map(|n| vec![n.to_string()]).unwrap_or_default()),
            ["\"DATE\""] => DataType::Date,
            ["\"YEAR\""] => DataType::Custom(ObjectName::from(vec![Ident::new(self.tx(c[0]))]), vec![]),
            ["\"DATETIME\"", "OptFieldLen"] => DataType::Datetime(field_len(c[1])),
            ["\"TIME\"", "OptFieldLen"] => DataType::Time(field_len(c[1]), TimezoneInfo::None),
            ["\"DECIMAL\"", "FloatOpt"] => DataType::Decimal(self.number_info(c[1])),
            ["\"SIGNED\"" | "\"UNSIGNED\"", "OptInteger"] => {
                let int = !self.empty(c[1]);
                match (word.as_str(), int) {
                    ("SIGNED", false) => DataType::Signed,
                    ("SIGNED", true) => DataType::SignedInteger,
                    ("UNSIGNED", false) => DataType::Unsigned,
                    _ => DataType::UnsignedInteger,
                }
            }
            ["\"JSON\""] => DataType::JSON,
            ["\"DOUBLE\""] => DataType::Double(ExactNumberInfo::None),
            ["\"DOUBLE\"", "\"PRECISION\""] => DataType::DoublePrecision,
            ["\"FLOAT\"", "FloatOpt"] => DataType::Float(self.number_info(c[1])),
            ["\"REAL\""] => DataType::Real,
            other => return no(&format!("cast type {other:?}")),
        };
        Ok((ty, None))
    }

    fn number_info(&self, id: NodeId) -> ExactNumberInfo {
        let n = |x: NodeId| self.tx(x).parse::<u64>().unwrap_or(0);
        match self.ch(id) {
            [] => ExactNumberInfo::None,
            [f] if self.k(*f) == "FieldLen" => ExactNumberInfo::Precision(n(self.ch(*f)[1])),
            [p] => {
                let pc = self.ch(*p);
                ExactNumberInfo::PrecisionAndScale(n(pc[1]), n(pc[3]) as i64)
            }
            _ => ExactNumberInfo::None,
        }
    }

    /// `OptBinary`: the character set a CHAR cast names.
    fn opt_binary(&self, id: NodeId) -> R<Option<ObjectName>> {
        let c = self.ch(id);
        match self.names(id).as_slice() {
            [] => Ok(None),
            ["\"BINARY\"", "OptCharset"] => {
                if self.empty(c[1]) {
                    return Ok(Some(ObjectName::from(vec![Ident::new("binary")])));
                }
                no("BINARY CHARSET")
            }
            ["CharsetKw", "CharsetName", "OptBinMod"] => {
                if !self.empty(c[2]) {
                    return no("CHARSET … BINARY");
                }
                Ok(Some(ObjectName::from(vec![self.name_ident(c[1])])))
            }
            other => no(&format!("cast charset {other:?}")),
        }
    }

    fn window_call(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let name = ObjectName::from(vec![Ident::new(self.tx(c[0]))]);
        let mut args = Vec::new();
        let mut null_treatment = None;
        let mut over = None;
        for &x in &c[2..] {
            match self.k(x) {
                "Expression" | "SimpleExpr" => args.push(self.expr(x)?),
                "OptLeadLagInfo" => {
                    for &y in self.ch(x) {
                        match self.k(y) {
                            "NumLiteral" => args.push(self.literal_num(y)),
                            "\"?\"" => args.push(self.leafish(y)?),
                            "OptLLDefault" => {
                                if let [_, e] = self.ch(y) {
                                    args.push(self.expr(*e)?);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "OptNullTreatment" => {
                    if let Some(&w) = self.ch(x).first() {
                        null_treatment = Some(if self.tx(w).eq_ignore_ascii_case("IGNORE") { NullTreatment::IgnoreNulls } else { NullTreatment::RespectNulls });
                    }
                }
                "OptFromFirstLast" => {
                    if !self.empty(x) {
                        return no("NTH_VALUE FROM FIRST");
                    }
                }
                "WindowingClause" => over = Some(self.windowing(x)?),
                _ => {}
            }
        }
        let mut f = func_parts(name);
        f.args = FunctionArguments::List(FunctionArgumentList { duplicate_treatment: None, args: args.into_iter().map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e))).collect(), clauses: vec![] });
        f.null_treatment = null_treatment;
        f.over = over;
        Ok(Expr::Function(f))
    }

    fn literal_num(&self, id: NodeId) -> Expr {
        self.number(self.first_leaf(id))
    }

    /// `OVER name` or `OVER (…)`.
    fn windowing(&self, id: NodeId) -> R<WindowType> {
        let spec = self.ch(id)[1];
        let inner = self.ch(spec)[0];
        match self.k(inner) {
            "WindowName" => Ok(WindowType::NamedWindow(self.ident(inner)?)),
            "WindowSpec" => Ok(WindowType::WindowSpec(self.window_spec(inner)?)),
            other => no(&format!("window {other}")),
        }
    }

    fn window_spec(&self, id: NodeId) -> R<WindowSpec> {
        let d = self.ch(self.ch(id)[1]);
        let window_name = match self.get(d[0], "WindowName") {
            Some(w) => Some(self.ident(w)?),
            None => None,
        };
        let mut partition_by = Vec::new();
        if let Some(l) = self.get(d[1], "ByList") {
            for item in self.list(l, "ByItem") {
                partition_by.push(self.expr(self.ch(item)[0])?);
            }
        }
        let order_by = match self.get(d[2], "ByList") {
            Some(l) => self.by_list(l)?,
            None => vec![],
        };
        let window_frame = if self.empty(d[3]) {
            None
        } else {
            let f = self.ch(d[3]);
            let units = match self.tx(f[0]).to_ascii_uppercase().as_str() {
                "ROWS" => WindowFrameUnits::Rows,
                "RANGE" => WindowFrameUnits::Range,
                _ => WindowFrameUnits::Groups,
            };
            let extent = self.ch(f[1])[0];
            if self.k(extent) == "WindowFrameStart" {
                Some(WindowFrame { units, start_bound: self.frame_bound(extent)?, end_bound: None })
            } else {
                let b = self.ch(extent);
                Some(WindowFrame { units, start_bound: self.frame_bound(b[1])?, end_bound: Some(self.frame_bound(b[3])?) })
            }
        };
        Ok(WindowSpec { window_name, partition_by, order_by, window_frame })
    }

    fn frame_bound(&self, id: NodeId) -> R<WindowFrameBound> {
        let c = self.ch(id);
        if c.len() == 1 {
            return self.frame_bound(c[0]);
        }
        let last = self.tx(*c.last().unwrap()).to_ascii_uppercase();
        let first = self.tx(c[0]).to_ascii_uppercase();
        if first == "CURRENT" {
            return Ok(WindowFrameBound::CurrentRow);
        }
        let value = if first == "UNBOUNDED" {
            None
        } else if first == "INTERVAL" {
            Some(Box::new(self.interval(c[1], c[2])?))
        } else {
            Some(Box::new(self.expr_any(c[0])?))
        };
        Ok(if last == "PRECEDING" { WindowFrameBound::Preceding(value) } else { WindowFrameBound::Following(value) })
    }

    // ---------------------------------------------------------------- literals and names

    fn literal(&self, id: NodeId) -> R<Expr> {
        let c = self.ch(id);
        let names = self.names(id);
        let v = |value: Value| Expr::Value(ValueWithSpan { value, span: self.span(id) });
        match names.as_slice() {
            ["\"FALSE\""] => Ok(v(Value::Boolean(false))),
            ["\"TRUE\""] => Ok(v(Value::Boolean(true))),
            ["\"NULL\""] => Ok(v(Value::Null)),
            ["\"hexadecimal literal\""] | ["\"bit literal\""] => Ok(v(self.hex_bit(c[0]))),
            [n] if n.contains("literal") => Ok(v(Value::Number(self.tx(c[0]).to_string(), false))),
            ["StringLiteral"] => Ok(self.string_literal(c[0])),
            ["\"UNDERSCORE_CHARSET\"", "\"string literal\""] if self.tx(c[0]).eq_ignore_ascii_case("N") => {
                let Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(text) | Value::DoubleQuotedString(text), .. }) = self.string_literal(c[1]) else { return no("national") };
                Ok(Expr::Value(ValueWithSpan { value: Value::NationalStringLiteral(text), span: self.span(id) }))
            }
            ["\"UNDERSCORE_CHARSET\"", what] => {
                let value = if *what == "\"string literal\"" { self.string_literal(c[1]) } else { Expr::Value(ValueWithSpan { value: self.hex_bit(c[1]), span: self.span(c[1]) }) };
                Ok(Expr::Prefixed { prefix: Ident::new(self.tx(c[0])), value: Box::new(value) })
            }
            other => no(&format!("literal {other:?}")),
        }
    }

    fn hex_bit(&self, id: NodeId) -> Value {
        let text = self.tx(id);
        let lower = text.to_ascii_lowercase();
        if let Some(h) = lower.strip_prefix("x'") {
            Value::HexStringLiteral(text[2..2 + h.len() - 1].to_string())
        } else if lower.starts_with("0x") {
            Value::HexStringLiteral(text[2..].to_string())
        } else if lower.starts_with("b'") {
            Value::SingleQuotedByteStringLiteral(text[2..text.len() - 1].to_string())
        } else {
            Value::SingleQuotedByteStringLiteral(text[2..].to_string())
        }
    }

    fn string_literal(&self, id: NodeId) -> Expr {
        // StringLiteral: stringLit | StringLiteral stringLit — adjacent strings join.
        let mut parts = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if self.t.node(n).leaf {
                parts.push(n);
            } else {
                stack.extend(self.ch(n).iter().rev());
            }
        }
        let first = self.tx(parts[0]);
        let quote = first.chars().next().unwrap();
        let mut value = String::new();
        for p in &parts {
            value.push_str(&self.unquote(self.tx(*p)));
        }
        let value = if first.starts_with(['N', 'n']) { Value::NationalStringLiteral(value) } else if quote == '"' { Value::DoubleQuotedString(value) } else { Value::SingleQuotedString(value) };
        Expr::Value(ValueWithSpan { value, span: self.span(id) })
    }

    fn string_value(&self, id: NodeId) -> Expr {
        self.string_literal(id)
    }

    fn unquote(&self, text: &str) -> String {
        let text = text.strip_prefix(['N', 'n']).unwrap_or(text);
        let q = text.chars().next().unwrap();
        let inner = &text[1..text.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == q && chars.peek() == Some(&q) {
                chars.next();
                out.push(q);
            } else if ch == '\\' && self.backslash {
                match chars.next() {
                    Some('0') => out.push('\0'),
                    Some('b') => out.push('\u{8}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('Z') => out.push('\u{1a}'),
                    Some('%') => out.push_str("\\%"),
                    Some('_') => out.push_str("\\_"),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn string_ident(&self, id: NodeId) -> Ident {
        let text = self.tx(id);
        Ident::with_quote(text.chars().next().unwrap(), self.unquote(text))
    }

    fn ident(&self, id: NodeId) -> R<Ident> {
        // Identifier: identifier | UnReservedKeyword | NotKeywordToken | TiDBKeyword
        let leaf = self.first_leaf(id);
        Ok(self.leaf_ident(leaf))
    }

    fn name_ident(&self, id: NodeId) -> Ident {
        let leaf = self.first_leaf(id);
        if self.k(leaf) == "\"string literal\"" {
            return self.string_ident(leaf);
        }
        self.leaf_ident(leaf)
    }

    fn first_leaf(&self, id: NodeId) -> NodeId {
        let mut n = id;
        while !self.t.node(n).leaf {
            n = self.ch(n)[0];
        }
        n
    }

    fn leaf_ident(&self, id: NodeId) -> Ident {
        let text = self.tx(id);
        if let Some(inner) = text.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
            Ident::with_quote('`', inner.replace("``", "`"))
        } else if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            Ident::with_quote('"', inner.replace("\"\"", "\""))
        } else {
            Ident::new(text)
        }
    }
}

fn func_parts(name: ObjectName) -> Function {
    Function { name, uses_odbc_syntax: false, parameters: FunctionArguments::None, args: FunctionArguments::None, within_group: vec![], filter: None, null_treatment: None, over: None }
}

/// `t1, t2, …` in parentheses or a multiple-table UPDATE: each later reference a CROSS JOIN, in
/// parentheses where it has joins of its own.
fn comma_join(refs: Vec<TableWithJoins>) -> TableWithJoins {
    let mut it = refs.into_iter();
    let mut first = it.next().unwrap();
    for r in it {
        let relation = if r.joins.is_empty() { r.relation } else { TableFactor::NestedJoin { table_with_joins: Box::new(r), alias: None } };
        first.joins.push(Join { relation, global: false, join_operator: JoinOperator::CrossJoin(JoinConstraint::None) });
    }
    first
}

fn func(name: ObjectName, args: Vec<Expr>) -> Expr {
    Expr::Function(Function {
        name,
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList { duplicate_treatment: None, args: args.into_iter().map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e))).collect(), clauses: vec![] }),
        within_group: vec![],
        filter: None,
        null_treatment: None,
        over: None,
    })
}

fn empty_select() -> Select {
    Select {
        select_token: AttachedToken::empty(),
        optimizer_hints: vec![],
        distinct: None,
        select_modifiers: None,
        top: None,
        top_before_distinct: false,
        projection: vec![],
        exclude: None,
        into: None,
        from: vec![],
        lateral_views: vec![],
        prewhere: None,
        selection: None,
        connect_by: vec![],
        group_by: GroupByExpr::Expressions(vec![], vec![]),
        cluster_by: vec![],
        distribute_by: vec![],
        sort_by: vec![],
        having: None,
        named_window: vec![],
        qualify: None,
        window_before_qualify: false,
        value_table_mode: None,
        flavor: SelectFlavor::Standard,
    }
}

fn plain(body: SetExpr) -> Query {
    Query { with: None, body: Box::new(body), order_by: None, limit_clause: None, fetch: None, locks: vec![], for_clause: None, settings: None, format_clause: None, pipe_operators: vec![] }
}

/// A query with nothing but its body is the body.
fn unwrap_plain(q: Query) -> SetExpr {
    if q.with.is_none() && q.order_by.is_none() && q.limit_clause.is_none() && q.locks.is_empty() && q.fetch.is_none() {
        *q.body
    } else {
        SetExpr::Query(Box::new(q))
    }
}

fn sel(q: &mut Query) -> &mut Select {
    match q.body.as_mut() {
        SetExpr::Select(s) => s,
        _ => unreachable!(),
    }
}

/// Left-associative set operations, INTERSECT binding tighter than UNION and EXCEPT.
fn combine(first: SetExpr, rest: Vec<(SetOperator, SetQuantifier, SetExpr)>) -> SetExpr {
    // Fold INTERSECT runs first.
    let mut terms: Vec<(Option<(SetOperator, SetQuantifier)>, SetExpr)> = vec![(None, first)];
    for (op, q, e) in rest {
        if op == SetOperator::Intersect {
            let (prev_op, prev) = terms.pop().unwrap();
            terms.push((prev_op, SetExpr::SetOperation { left: Box::new(prev), op, set_quantifier: q, right: Box::new(e) }));
        } else {
            terms.push((Some((op, q)), e));
        }
    }
    let mut it = terms.into_iter();
    let mut acc = it.next().unwrap().1;
    for (op, e) in it {
        let (op, q) = op.unwrap();
        acc = SetExpr::SetOperation { left: Box::new(acc), op, set_quantifier: q, right: Box::new(e) };
    }
    acc
}
