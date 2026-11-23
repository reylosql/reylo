//! A PL/pgSQL function as PL/pgSQL's compiler builds it (`plpgsql.h`), and the JSON libpg_query writes
//! of one (`pg_query_json_plpgsql.c`), field for field.

/// How an expression's text is read (`RawParseMode`).
pub const PARSE_DEFAULT: i32 = 0;
pub const PARSE_TYPE_NAME: i32 = 1;
pub const PARSE_PLPGSQL_EXPR: i32 = 2;
pub const PARSE_PLPGSQL_ASSIGN1: i32 = 3;
pub const PARSE_PLPGSQL_ASSIGN2: i32 = 4;
pub const PARSE_PLPGSQL_ASSIGN3: i32 = 5;

/// `CURSOR_OPT_*`.
pub const CURSOR_SCROLL: i32 = 0x0002;
pub const CURSOR_NO_SCROLL: i32 = 0x0004;
pub const CURSOR_FAST_PLAN: i32 = 0x0100;

/// `FETCH_*`, and `FETCH_ALL`.
pub const FETCH_FORWARD: i32 = 0;
pub const FETCH_BACKWARD: i32 = 1;
pub const FETCH_ABSOLUTE: i32 = 2;
pub const FETCH_RELATIVE: i32 = 3;
pub const FETCH_ALL: i64 = i64::MAX;

/// The message levels a RAISE names (`elog.h`).
pub const DEBUG1: i32 = 14;
pub const LOG: i32 = 15;
pub const INFO: i32 = 17;
pub const NOTICE: i32 = 18;
pub const WARNING: i32 = 19;
pub const ERROR: i32 = 21;

/// A type, as far as a datum's JSON shows it: its name, and what a variable of it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Type {
    pub typname: String,
    pub typoid: u32,
    pub ttype: TType,
    pub typisarray: bool,
    /// Whether the type takes a collation.
    pub collatable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TType {
    Scalar,
    Rec,
    Pseudo,
}

/// An expression or statement's text, kept as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    pub query: String,
    pub parse_mode: i32,
}

#[derive(Clone, Debug)]
pub struct Var {
    pub refname: String,
    pub lineno: i32,
    pub datatype: Option<Type>,
    pub isconst: bool,
    pub notnull: bool,
    pub default_val: Option<Expr>,
    pub cursor_explicit_expr: Option<Expr>,
    pub cursor_explicit_argrow: i32,
    pub cursor_options: i32,
    /// Which trigger variable this stands for (`PLPGSQL_PROMISE_*`), or 0 for a plain variable.
    pub promise: i32,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub refname: String,
    pub lineno: i32,
    pub fields: Vec<(String, i32)>,
}

#[derive(Clone, Debug)]
pub struct Rec {
    pub refname: String,
    pub dno: i32,
    pub lineno: i32,
    pub isconst: bool,
    pub notnull: bool,
    pub default_val: Option<Expr>,
    /// The first of its fields' datums, which chain through `nextfield`; -1 for none.
    pub firstfield: i32,
}

#[derive(Clone, Debug)]
pub struct RecField {
    pub fieldname: String,
    pub recparentno: i32,
    pub nextfield: i32,
}

/// A function's datum: a variable, a row of variables, a record, or a field of one.
#[derive(Clone, Debug)]
pub enum Datum {
    Var(Var),
    Row(Row),
    Rec(Rec),
    RecField(RecField),
}

impl Datum {
    pub fn is_var(&self) -> bool {
        matches!(self, Datum::Var(v) if v.promise == 0)
    }
    pub fn is_promise(&self) -> bool {
        matches!(self, Datum::Var(v) if v.promise != 0)
    }
}

#[derive(Clone, Debug)]
pub struct Condition {
    pub condname: String,
}

#[derive(Clone, Debug)]
pub struct Exception {
    pub conditions: Vec<Condition>,
    pub action: Vec<Stmt>,
}

#[derive(Clone, Debug, Default)]
pub struct ExceptionBlock {
    pub sqlstate_varno: i32,
    pub sqlerrm_varno: i32,
    pub exc_list: Vec<Exception>,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub lineno: i32,
    pub label: Option<String>,
    pub body: Vec<Stmt>,
    pub exceptions: Option<ExceptionBlock>,
    pub initvarnos: Vec<i32>,
}

#[derive(Clone, Debug)]
pub struct IfElsif {
    pub lineno: i32,
    pub cond: Expr,
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct CaseWhen {
    pub lineno: i32,
    pub expr: Expr,
    pub stmts: Vec<Stmt>,
}

/// A loop over rows: a query's (FORS), a cursor's (FORC) or a dynamic query's (DYNFORS).
#[derive(Clone, Debug)]
pub struct Forq {
    pub lineno: i32,
    pub label: Option<String>,
    /// The datum number of the loop's variable.
    pub var: i32,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Fetch {
    pub lineno: i32,
    pub target: Option<i32>,
    pub curvar: i32,
    pub direction: i32,
    pub how_many: i64,
    pub expr: Option<Expr>,
    pub is_move: bool,
    pub returns_multiple_rows: bool,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Block(Block),
    Assign { lineno: i32, varno: i32, expr: Expr },
    If { lineno: i32, cond: Expr, then_body: Vec<Stmt>, elsif_list: Vec<IfElsif>, else_body: Vec<Stmt> },
    Case { lineno: i32, t_expr: Option<Expr>, t_varno: i32, case_when_list: Vec<CaseWhen>, have_else: bool, else_stmts: Vec<Stmt> },
    Loop { lineno: i32, label: Option<String>, body: Vec<Stmt> },
    While { lineno: i32, label: Option<String>, cond: Expr, body: Vec<Stmt> },
    ForI { lineno: i32, label: Option<String>, var: i32, lower: Expr, upper: Expr, step: Option<Expr>, reverse: bool, body: Vec<Stmt> },
    ForS { forq: Forq, query: Expr },
    ForC { forq: Forq, curvar: i32, argquery: Option<Expr> },
    DynForS { forq: Forq, query: Expr, params: Vec<Expr> },
    ForeachA { lineno: i32, label: Option<String>, varno: i32, slice: i32, expr: Expr, body: Vec<Stmt> },
    Exit { lineno: i32, is_exit: bool, label: Option<String>, cond: Option<Expr> },
    Return { lineno: i32, expr: Option<Expr>, retvarno: i32 },
    ReturnNext { lineno: i32, expr: Option<Expr>, retvarno: i32 },
    ReturnQuery { lineno: i32, query: Option<Expr>, dynquery: Option<Expr>, params: Vec<Expr> },
    Raise { lineno: i32, elog_level: i32, condname: Option<String>, message: Option<String>, params: Vec<Expr>, options: Vec<(i32, Expr)> },
    Assert { lineno: i32, cond: Expr, message: Option<Expr> },
    ExecSql { lineno: i32, sqlstmt: Expr, into: bool, strict: bool, target: Option<i32> },
    DynExecute { lineno: i32, query: Expr, into: bool, strict: bool, target: Option<i32>, params: Vec<Expr> },
    GetDiag { lineno: i32, is_stacked: bool, diag_items: Vec<(i32, i32)> },
    Open { lineno: i32, curvar: i32, cursor_options: i32, argquery: Option<Expr>, query: Option<Expr>, dynquery: Option<Expr>, params: Vec<Expr> },
    Fetch(Fetch),
    Close { lineno: i32, curvar: i32 },
    Perform { lineno: i32, expr: Expr },
    Call { lineno: i32, expr: Expr, is_call: bool },
    Commit { lineno: i32, chain: bool },
    Rollback { lineno: i32, chain: bool },
}

/// A compiled function: its datums, numbered as the statements refer to them, and its body.
#[derive(Clone, Debug)]
pub struct Function {
    pub new_varno: i32,
    pub old_varno: i32,
    pub datums: Vec<Datum>,
    /// None for a function in another language, which libpg_query writes with no body.
    pub action: Option<Block>,
}

/// GET DIAGNOSTICS item kinds (`PLpgSQL_getdiag_kind`), and how a statement names each.
pub const GETDIAG_KINDS: [&str; 13] = ["ROW_COUNT", "PG_ROUTINE_OID", "PG_CONTEXT", "PG_EXCEPTION_CONTEXT", "PG_EXCEPTION_DETAIL", "PG_EXCEPTION_HINT", "RETURNED_SQLSTATE", "COLUMN_NAME", "CONSTRAINT_NAME", "PG_DATATYPE_NAME", "MESSAGE_TEXT", "TABLE_NAME", "SCHEMA_NAME"];

/// The JSON libpg_query writes, built as its macros build it: a field is followed by a comma, and
/// the comma before a closing bracket is removed.
struct Out(String);

impl Out {
    fn trim(&mut self) {
        if self.0.ends_with(',') {
            self.0.pop();
        }
    }
    fn node(&mut self, label: &str) {
        self.0.push('"');
        self.0.push_str(label);
        self.0.push_str("\":{");
    }
    fn int(&mut self, name: &str, value: i64) {
        if value != 0 {
            self.0.push_str(&format!("\"{name}\":{value},"));
        }
    }
    fn enumeration(&mut self, name: &str, value: i64) {
        self.0.push_str(&format!("\"{name}\":{value},"));
    }
    fn boolean(&mut self, name: &str, value: bool) {
        if value {
            self.0.push_str(&format!("\"{name}\":true,"));
        }
    }
    fn string(&mut self, name: &str, value: Option<&str>) {
        if let Some(v) = value {
            self.0.push_str(&format!("\"{name}\":"));
            token(&mut self.0, v);
            self.0.push(',');
        }
    }
    /// `WRITE_OBJ_FIELD`: the field, then the object `write` fills.
    fn object(&mut self, name: &str, write: impl FnOnce(&mut Out)) {
        self.0.push_str(&format!("\"{name}\":{{"));
        write(self);
        self.trim();
        self.0.push_str("}},");
    }
    fn list<T>(&mut self, name: &str, items: &[T], write: impl Fn(&mut Out, &T)) {
        if items.is_empty() {
            return;
        }
        self.0.push_str(&format!("\"{name}\":["));
        for item in items {
            self.0.push('{');
            write(self, item);
            self.trim();
            self.0.push_str("}},");
        }
        self.trim();
        self.0.push_str("],");
    }
    fn statements(&mut self, name: &str, stmts: &[Stmt], datums: &[Datum]) {
        if stmts.is_empty() {
            return;
        }
        self.0.push_str(&format!("\"{name}\":["));
        for s in stmts {
            self.stmt(s, datums);
        }
        self.trim();
        self.0.push_str("],");
    }
    fn expr(&mut self, name: &str, e: &Option<Expr>) {
        if let Some(e) = e {
            self.object(name, |o| write_expr(o, e));
        }
    }
    /// `WRITE_VARIABLE_FIELD`: a record, a variable or a row.
    fn variable(&mut self, name: &str, varno: Option<i32>, datums: &[Datum]) {
        if let Some(d) = varno.and_then(|n| datums.get(n as usize)) {
            self.object(name, |o| write_datum(o, d));
        }
    }

    fn stmt(&mut self, s: &Stmt, datums: &[Datum]) {
        self.0.push('{');
        match s {
            Stmt::Block(b) => write_block(self, b, datums),
            Stmt::Assign { lineno, varno, expr } => {
                self.node("PLpgSQL_stmt_assign");
                self.int("lineno", *lineno as i64);
                self.int("varno", *varno as i64);
                self.object("expr", |o| write_expr(o, expr));
            }
            Stmt::If { lineno, cond, then_body, elsif_list, else_body } => {
                self.node("PLpgSQL_stmt_if");
                self.int("lineno", *lineno as i64);
                self.object("cond", |o| write_expr(o, cond));
                self.statements("then_body", then_body, datums);
                self.list("elsif_list", elsif_list, |o, e| {
                    o.node("PLpgSQL_if_elsif");
                    o.int("lineno", e.lineno as i64);
                    o.object("cond", |o| write_expr(o, &e.cond));
                    o.statements("stmts", &e.stmts, datums);
                });
                self.statements("else_body", else_body, datums);
            }
            Stmt::Case { lineno, t_expr, t_varno, case_when_list, have_else, else_stmts } => {
                self.node("PLpgSQL_stmt_case");
                self.int("lineno", *lineno as i64);
                self.expr("t_expr", t_expr);
                self.int("t_varno", *t_varno as i64);
                self.list("case_when_list", case_when_list, |o, w| {
                    o.node("PLpgSQL_case_when");
                    o.int("lineno", w.lineno as i64);
                    o.object("expr", |o| write_expr(o, &w.expr));
                    o.statements("stmts", &w.stmts, datums);
                });
                self.boolean("have_else", *have_else);
                self.statements("else_stmts", else_stmts, datums);
            }
            Stmt::Loop { lineno, label, body } => {
                self.node("PLpgSQL_stmt_loop");
                self.int("lineno", *lineno as i64);
                self.string("label", label.as_deref());
                self.statements("body", body, datums);
            }
            Stmt::While { lineno, label, cond, body } => {
                self.node("PLpgSQL_stmt_while");
                self.int("lineno", *lineno as i64);
                self.string("label", label.as_deref());
                self.object("cond", |o| write_expr(o, cond));
                self.statements("body", body, datums);
            }
            Stmt::ForI { lineno, label, var, lower, upper, step, reverse, body } => {
                self.node("PLpgSQL_stmt_fori");
                self.int("lineno", *lineno as i64);
                self.string("label", label.as_deref());
                if let Some(Datum::Var(v)) = datums.get(*var as usize) {
                    self.object("var", |o| write_var(o, v));
                }
                self.object("lower", |o| write_expr(o, lower));
                self.object("upper", |o| write_expr(o, upper));
                self.expr("step", step);
                self.boolean("reverse", *reverse);
                self.statements("body", body, datums);
            }
            Stmt::ForS { forq, query } => {
                self.node("PLpgSQL_stmt_fors");
                self.forq(forq, datums);
                self.object("query", |o| write_expr(o, query));
            }
            Stmt::ForC { forq, curvar, argquery } => {
                self.node("PLpgSQL_stmt_forc");
                self.forq(forq, datums);
                self.int("curvar", *curvar as i64);
                self.expr("argquery", argquery);
            }
            Stmt::DynForS { forq, query, params } => {
                self.node("PLpgSQL_stmt_dynfors");
                self.forq(forq, datums);
                self.object("query", |o| write_expr(o, query));
                self.list("params", params, write_expr);
            }
            Stmt::ForeachA { lineno, label, varno, slice, expr, body } => {
                self.node("PLpgSQL_stmt_foreach_a");
                self.int("lineno", *lineno as i64);
                self.string("label", label.as_deref());
                self.int("varno", *varno as i64);
                self.int("slice", *slice as i64);
                self.object("expr", |o| write_expr(o, expr));
                self.statements("body", body, datums);
            }
            Stmt::Exit { lineno, is_exit, label, cond } => {
                self.node("PLpgSQL_stmt_exit");
                self.int("lineno", *lineno as i64);
                self.boolean("is_exit", *is_exit);
                self.string("label", label.as_deref());
                self.expr("cond", cond);
            }
            Stmt::Return { lineno, expr, retvarno } => {
                self.node("PLpgSQL_stmt_return");
                self.int("lineno", *lineno as i64);
                self.expr("expr", expr);
                self.int("retvarno", *retvarno as i64);
            }
            Stmt::ReturnNext { lineno, expr, .. } => {
                self.node("PLpgSQL_stmt_return_next");
                self.int("lineno", *lineno as i64);
                self.expr("expr", expr);
            }
            Stmt::ReturnQuery { lineno, query, dynquery, params } => {
                self.node("PLpgSQL_stmt_return_query");
                self.int("lineno", *lineno as i64);
                self.expr("query", query);
                self.expr("dynquery", dynquery);
                self.list("params", params, write_expr);
            }
            Stmt::Raise { lineno, elog_level, condname, message, params, options } => {
                self.node("PLpgSQL_stmt_raise");
                self.int("lineno", *lineno as i64);
                self.int("elog_level", *elog_level as i64);
                self.string("condname", condname.as_deref());
                self.string("message", message.as_deref());
                self.list("params", params, write_expr);
                self.list("options", options, |o, (kind, e)| {
                    o.node("PLpgSQL_raise_option");
                    o.enumeration("opt_type", *kind as i64);
                    o.object("expr", |o| write_expr(o, e));
                });
            }
            Stmt::Assert { lineno, cond, message } => {
                self.node("PLpgSQL_stmt_assert");
                self.int("lineno", *lineno as i64);
                self.object("cond", |o| write_expr(o, cond));
                self.expr("message", message);
            }
            Stmt::ExecSql { lineno, sqlstmt, into, strict, target } => {
                self.node("PLpgSQL_stmt_execsql");
                self.int("lineno", *lineno as i64);
                self.object("sqlstmt", |o| write_expr(o, sqlstmt));
                self.boolean("into", *into);
                self.boolean("strict", *strict);
                self.variable("target", *target, datums);
            }
            Stmt::DynExecute { lineno, query, into, strict, target, params } => {
                self.node("PLpgSQL_stmt_dynexecute");
                self.int("lineno", *lineno as i64);
                self.object("query", |o| write_expr(o, query));
                self.boolean("into", *into);
                self.boolean("strict", *strict);
                self.variable("target", *target, datums);
                self.list("params", params, write_expr);
            }
            Stmt::GetDiag { lineno, is_stacked, diag_items } => {
                self.node("PLpgSQL_stmt_getdiag");
                self.int("lineno", *lineno as i64);
                self.boolean("is_stacked", *is_stacked);
                self.list("diag_items", diag_items, |o, (target, kind)| {
                    o.node("PLpgSQL_diag_item");
                    o.string("kind", Some(GETDIAG_KINDS[*kind as usize]));
                    o.int("target", *target as i64);
                });
            }
            Stmt::Open { lineno, curvar, cursor_options, argquery, query, dynquery, params } => {
                self.node("PLpgSQL_stmt_open");
                self.int("lineno", *lineno as i64);
                self.int("curvar", *curvar as i64);
                self.int("cursor_options", *cursor_options as i64);
                self.expr("argquery", argquery);
                self.expr("query", query);
                self.expr("dynquery", dynquery);
                self.list("params", params, write_expr);
            }
            Stmt::Fetch(f) => {
                self.node("PLpgSQL_stmt_fetch");
                self.int("lineno", f.lineno as i64);
                self.variable("target", f.target, datums);
                self.int("curvar", f.curvar as i64);
                self.enumeration("direction", f.direction as i64);
                self.int("how_many", f.how_many);
                self.expr("expr", &f.expr);
                self.boolean("is_move", f.is_move);
                self.boolean("returns_multiple_rows", f.returns_multiple_rows);
            }
            Stmt::Close { lineno, curvar } => {
                self.node("PLpgSQL_stmt_close");
                self.int("lineno", *lineno as i64);
                self.int("curvar", *curvar as i64);
            }
            Stmt::Perform { lineno, expr } => {
                self.node("PLpgSQL_stmt_perform");
                self.int("lineno", *lineno as i64);
                self.object("expr", |o| write_expr(o, expr));
            }
            Stmt::Call { lineno, expr, is_call } => {
                self.node("PLpgSQL_stmt_call");
                self.int("lineno", *lineno as i64);
                self.object("expr", |o| write_expr(o, expr));
                self.boolean("is_call", *is_call);
            }
            Stmt::Commit { lineno, chain } => {
                self.node("PLpgSQL_stmt_commit");
                self.int("lineno", *lineno as i64);
                self.boolean("chain", *chain);
            }
            Stmt::Rollback { lineno, chain } => {
                self.node("PLpgSQL_stmt_rollback");
                self.int("lineno", *lineno as i64);
                self.boolean("chain", *chain);
            }
        }
        self.trim();
        self.0.push_str("}},");
    }

    fn forq(&mut self, f: &Forq, datums: &[Datum]) {
        self.int("lineno", f.lineno as i64);
        self.string("label", f.label.as_deref());
        self.variable("var", Some(f.var), datums);
        self.statements("body", &f.body, datums);
    }
}

fn write_expr(o: &mut Out, e: &Expr) {
    o.node("PLpgSQL_expr");
    o.string("query", Some(&e.query));
    o.enumeration("parseMode", e.parse_mode as i64);
}

fn write_block(o: &mut Out, b: &Block, datums: &[Datum]) {
    o.node("PLpgSQL_stmt_block");
    o.int("lineno", b.lineno as i64);
    o.string("label", b.label.as_deref());
    o.statements("body", &b.body, datums);
    if let Some(x) = &b.exceptions {
        o.object("exceptions", |o| {
            o.node("PLpgSQL_exception_block");
            o.list("exc_list", &x.exc_list, |o, e| {
                o.node("PLpgSQL_exception");
                o.0.push_str("\"conditions\":[");
                for c in &e.conditions {
                    o.0.push('{');
                    o.node("PLpgSQL_condition");
                    o.string("condname", Some(&c.condname));
                    o.trim();
                    o.0.push_str("}},");
                }
                o.trim();
                o.0.push_str("],");
                o.statements("action", &e.action, datums);
            });
        });
    }
}

fn write_var(o: &mut Out, v: &Var) {
    o.node("PLpgSQL_var");
    o.string("refname", Some(&v.refname));
    o.int("lineno", v.lineno as i64);
    if let Some(t) = &v.datatype {
        o.object("datatype", |o| {
            o.node("PLpgSQL_type");
            o.string("typname", Some(&t.typname));
        });
    }
    o.boolean("isconst", v.isconst);
    o.boolean("notnull", v.notnull);
    o.expr("default_val", &v.default_val);
    o.expr("cursor_explicit_expr", &v.cursor_explicit_expr);
    o.int("cursor_explicit_argrow", v.cursor_explicit_argrow as i64);
    o.int("cursor_options", v.cursor_options as i64);
}

fn write_datum(o: &mut Out, d: &Datum) {
    match d {
        Datum::Var(v) => write_var(o, v),
        Datum::Row(r) => {
            o.node("PLpgSQL_row");
            o.string("refname", Some(&r.refname));
            o.int("lineno", r.lineno as i64);
            o.0.push_str("\"fields\":[");
            for (name, varno) in &r.fields {
                o.0.push('{');
                o.0.push_str("\"name\":");
                token(&mut o.0, name);
                o.0.push(',');
                o.int("varno", *varno as i64);
                o.trim();
                o.0.push_str("},");
            }
            o.trim();
            o.0.push_str("],");
        }
        Datum::Rec(r) => {
            o.node("PLpgSQL_rec");
            o.string("refname", Some(&r.refname));
            o.int("dno", r.dno as i64);
            o.int("lineno", r.lineno as i64);
        }
        Datum::RecField(f) => {
            o.node("PLpgSQL_recfield");
            o.string("fieldname", Some(&f.fieldname));
            o.int("recparentno", f.recparentno as i64);
        }
    }
}

/// `_outToken`'s JSON string: a quoted string, escaped as libpg_query's `_outToken` does.
fn token(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Function {
    /// The function as libpg_query's `plpgsqlToJSON` writes it.
    pub fn to_json(&self) -> String {
        let mut o = Out(String::with_capacity(1024));
        o.0.push('{');
        o.node("PLpgSQL_function");
        o.int("new_varno", self.new_varno as i64);
        o.int("old_varno", self.old_varno as i64);
        o.0.push_str("\"datums\":[");
        for d in &self.datums {
            o.0.push('{');
            write_datum(&mut o, d);
            o.trim();
            o.0.push_str("}},");
        }
        o.trim();
        o.0.push_str("],");
        if let Some(action) = &self.action {
            o.object("action", |o| write_block(o, action, &self.datums));
        }
        o.trim();
        o.0.push_str("}}");
        o.0
    }
}
