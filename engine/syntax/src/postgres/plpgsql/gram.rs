//! PL/pgSQL's grammar (`pl_gram.y`): Bison's tables driven as Bison's skeleton drives them, so that
//! an action reads tokens and sees the lookahead exactly where Postgres's would, and the actions
//! and the routines they call, ported one for one.

use super::comp::parse_err_condition;
use super::compile::{build_datatype, condition_known, format_type, lookup_type, quote_identifier, Code, Compiler, Lookup, Tok, TypeRef, Val, DOT, EMPTY, EOF, INT4OID, LABEL_BLOCK, LABEL_LOOP, LABEL_OTHER, LBRACKET, NS_LABEL, PERCENT, R, RBRACKET, RECORDOID, REFCURSOROID, TEXTOID, VOIDOID};
use super::nodes::*;
use super::scanner::is_unreserved_keyword;
use super::tables::{t, CHECK, DEFACT, DEFGOTO, FINAL, LAST, NTOKENS, PACT, PACT_NINF, PGOTO, R1, R2, RULES, TABLE};

/// How deep Bison's stacks may grow (`YYMAXDEPTH`).
const MAX_DEPTH: usize = 10_000;

/// A grammar symbol's value (`YYSTYPE`, as each `%type` uses it).
#[derive(Default)]
enum SV {
    #[default]
    None,
    Tok(Tok),
    Str(Option<String>),
    Bool(bool),
    Int(i32),
    VarName(String, i32),
    ForVar { name: String, lineno: i32, scalar: Option<i32>, row: Option<i32> },
    DeclHdr { label: Option<String>, initvarnos: Vec<i32> },
    LoopBody { stmts: Vec<Stmt>, end_label: Option<String> },
    Stmts(Vec<Stmt>),
    Stmt(Option<Stmt>),
    Expr(Option<Expr>),
    Type(Type),
    Dnos(Vec<i32>),
    NsItem(usize),
    Elsifs(Vec<IfElsif>),
    CaseWhen(CaseWhen),
    CaseWhens(Vec<CaseWhen>),
    OptElse(Option<Vec<Stmt>>),
    Exceptions(Vec<Exception>),
    Exception(Exception),
    Conditions(Vec<Condition>),
    ExcBlock(Option<ExceptionBlock>),
    DiagItems(Vec<(i32, i32)>),
    Fetch(Fetch),
}

impl SV {
    fn tok(self) -> Tok {
        match self {
            SV::Tok(t) => t,
            _ => Tok::default(),
        }
    }
    fn str(self) -> Option<String> {
        match self {
            SV::Str(s) => s,
            SV::Tok(t) => Some(t.str()),
            _ => None,
        }
    }
    fn bool(self) -> bool {
        matches!(self, SV::Bool(true))
    }
    fn int(self) -> i32 {
        match self {
            SV::Int(i) => i,
            SV::Tok(Tok { val: Val::Int(i), .. }) => i,
            _ => 0,
        }
    }
    fn stmts(self) -> Vec<Stmt> {
        match self {
            SV::Stmts(s) => s,
            _ => Vec::new(),
        }
    }
    fn stmt(self) -> Option<Stmt> {
        match self {
            SV::Stmt(s) => s,
            _ => None,
        }
    }
    fn expr(self) -> Option<Expr> {
        match self {
            SV::Expr(e) => e,
            _ => None,
        }
    }
    fn dnos(self) -> Vec<i32> {
        match self {
            SV::Dnos(d) => d,
            _ => Vec::new(),
        }
    }
}

/// The datum a T_DATUM token names, with the names that named it.
fn wdatum(tok: &Tok) -> (i32, Option<String>, bool, Vec<String>) {
    match &tok.val {
        Val::Datum { dno, ident, quoted, idents } => (*dno, ident.clone(), *quoted, idents.clone()),
        _ => (-1, None, false, Vec::new()),
    }
}

/// `NameOfDatum`.
fn name_of_datum(tok: &Tok) -> String {
    let (_, ident, _, idents) = wdatum(tok);
    ident.unwrap_or_else(|| idents.join("."))
}

fn word_is_not_variable(ident: &str) -> String {
    format!("\"{ident}\" is not a known variable")
}

fn cword_is_not_variable(idents: &[String]) -> String {
    format!("\"{}\" is not a known variable", idents.join("."))
}

fn word_of(tok: &Tok) -> String {
    match &tok.val {
        Val::Word(ident) => ident.clone(),
        _ => tok.str(),
    }
}

fn cword_of(tok: &Tok) -> Vec<String> {
    match &tok.val {
        Val::CWord(idents) => idents.clone(),
        _ => Vec::new(),
    }
}

/// `check_labels`.
fn check_labels(start: &Option<String>, end: &Option<String>) -> R<()> {
    if let Some(end) = end {
        match start {
            None => return Err(format!("end label \"{end}\" specified for unlabeled block")),
            Some(start) if start != end => return Err(format!("end label \"{end}\" differs from block's label \"{start}\"")),
            _ => {}
        }
    }
    Ok(())
}

/// `check_raise_parameters`.
fn check_raise_parameters(message: &Option<String>, params: usize) -> R<()> {
    let Some(message) = message else { return Ok(()) };
    let b = message.as_bytes();
    let mut expected = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if b.get(i + 1) == Some(&b'%') {
                i += 1;
            } else {
                expected += 1;
            }
        }
        i += 1;
    }
    if expected < params {
        return Err("too many parameters specified for RAISE".to_string());
    }
    if expected > params {
        return Err("too few parameters specified for RAISE".to_string());
    }
    Ok(())
}

fn valid_sqlstate(s: &str) -> bool {
    s.len() == 5 && s.bytes().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

fn translate(code: Code) -> i32 {
    if (code as i32) < NTOKENS {
        code as i32
    } else {
        2
    }
}

impl Compiler<'_> {
    /// `plpgsql_yyparse`, as Bison's skeleton runs the tables.
    pub fn yyparse(&mut self) -> R<()> {
        let mut states: Vec<i32> = vec![0];
        let mut vals: Vec<SV> = vec![SV::None];
        let mut locs: Vec<i32> = vec![0];
        self.yychar = EMPTY;
        let mut state = 0i32;
        loop {
            if state == FINAL {
                return Ok(());
            }
            let rule;
            let n = PACT[state as usize];
            if n == PACT_NINF {
                rule = DEFACT[state as usize];
            } else {
                if self.yychar == EMPTY {
                    self.yychar = self.yylex()?;
                }
                let token = translate(self.yychar);
                let i = n + token;
                if !(0..=LAST).contains(&i) || CHECK[i as usize] != token {
                    rule = DEFACT[state as usize];
                } else {
                    let a = TABLE[i as usize];
                    if a <= 0 {
                        rule = -a;
                    } else {
                        state = a;
                        states.push(state);
                        vals.push(SV::Tok(self.cur.clone()));
                        locs.push(self.cur.loc);
                        self.yychar = EMPTY;
                        if states.len() >= MAX_DEPTH {
                            return Err(self.yyerror("memory exhausted"));
                        }
                        continue;
                    }
                }
            }
            if rule == 0 {
                return Err(self.yyerror("syntax error"));
            }
            let rule = rule as usize;
            let len = R2[rule] as usize;
            let base = vals.len() - len;
            let here = if len > 0 { locs[base] } else { *locs.last().unwrap_or(&0) };
            let value = match self.action(rule, &mut vals, &locs, base)? {
                Some(v) => v,
                None if len > 0 => std::mem::take(&mut vals[base]),
                None => SV::None,
            };
            states.truncate(base);
            vals.truncate(base);
            locs.truncate(base);
            let lhs = (R1[rule] - NTOKENS) as usize;
            let top = *states.last().unwrap_or(&0);
            let i = PGOTO[lhs] + top;
            state = if (0..=LAST).contains(&i) && CHECK[i as usize] == top { TABLE[i as usize] } else { DEFGOTO[lhs] };
            states.push(state);
            vals.push(value);
            locs.push(here);
            if states.len() >= MAX_DEPTH {
                return Err(self.yyerror("memory exhausted"));
            }
        }
    }

    /// A rule's action: its value, or None to keep `$1`.
    fn action(&mut self, rule: usize, v: &mut [SV], l: &[i32], base: usize) -> R<Option<SV>> {
        macro_rules! take {
            ($k:expr) => {
                std::mem::take(&mut v[base + $k - 1])
            };
        }
        macro_rules! at {
            ($k:expr) => {
                l[base + $k - 1]
            };
        }
        let (lhs, rhs) = RULES[rule];
        let first = rhs.first().copied().unwrap_or("");
        Ok(Some(match lhs {
            "pl_function" => {
                if let Some(Stmt::Block(b)) = take!(2).stmt() {
                    self.result = Some(b);
                }
                SV::None
            }
            "comp_option" => {
                if rhs.get(1) == Some(&"K_PRINT_STRICT_PARAMS") {
                    let value = take!(3).str().unwrap_or_default();
                    if value != "on" && value != "off" {
                        return Err(format!("unrecognized print_strict_params option {value}"));
                    }
                }
                SV::None
            }
            "option_value" => SV::Str(Some(take!(1).tok().str())),
            "pl_block" => {
                let SV::DeclHdr { label, initvarnos } = take!(1) else { return Err("bad declaration header".into()) };
                let end_label = take!(6).str();
                let block = Block { lineno: self.lineno(at!(2)), label: label.clone(), body: take!(3).stmts(), exceptions: match take!(4) {
                    SV::ExcBlock(e) => e,
                    _ => None,
                }, initvarnos };
                check_labels(&label, &end_label)?;
                self.ns_pop();
                SV::Stmt(Some(Stmt::Block(block)))
            }
            "decl_sect" => {
                self.lookup = Lookup::Normal;
                let label = take!(1).str();
                let initvarnos = if rhs.len() == 3 { self.add_initdatums() } else { Vec::new() };
                SV::DeclHdr { label, initvarnos }
            }
            "decl_start" => {
                self.add_initdatums();
                self.lookup = Lookup::Declare;
                SV::None
            }
            "decl_stmt" if first == "LESS_LESS" => return Err("block label must be placed before DECLARE, not after".into()),
            "decl_statement" if rhs.len() == 6 => {
                let SV::VarName(name, lineno) = take!(1) else { return Err("bad declaration".into()) };
                let isconst = take!(2).bool();
                let SV::Type(dtype) = take!(3) else { return Err("bad data type".into()) };
                let collate = take!(4).bool();
                let notnull = take!(5).bool();
                let default_val = take!(6).expr();
                if collate && !dtype.collatable {
                    return Err(format!("collations are not supported by type {}", format_type(dtype.typoid)?));
                }
                let dno = self.build_variable(&name, lineno, dtype, true)?;
                let refname = match &mut self.datums[dno as usize] {
                    Datum::Var(var) => {
                        var.isconst = isconst;
                        var.notnull = notnull;
                        var.default_val = default_val.clone();
                        var.refname.clone()
                    }
                    Datum::Rec(rec) => {
                        rec.isconst = isconst;
                        rec.notnull = notnull;
                        rec.default_val = default_val.clone();
                        rec.refname.clone()
                    }
                    _ => name,
                };
                if notnull && default_val.is_none() {
                    return Err(format!("variable \"{refname}\" must have a default value, since it's declared NOT NULL"));
                }
                SV::None
            }
            "decl_statement" if rhs.get(1) == Some(&"K_ALIAS") => {
                let SV::VarName(name, _) = take!(1) else { return Err("bad declaration".into()) };
                let SV::NsItem(item) = take!(4) else { return Err("bad alias".into()) };
                let (kind, itemno) = (self.ns[item].kind, self.ns[item].itemno);
                self.ns_additem(kind, itemno, &name);
                SV::None
            }
            "$@1" => {
                if let SV::VarName(name, _) = &v[base - 3] {
                    let name = name.clone();
                    self.ns_push(Some(&name), LABEL_OTHER);
                }
                SV::None
            }
            "decl_statement" => {
                self.ns_pop();
                let SV::VarName(name, lineno) = take!(1) else { return Err("bad declaration".into()) };
                let options = take!(2).int();
                let args = match take!(5) {
                    SV::Int(dno) => dno,
                    _ => -1,
                };
                let query = take!(7).expr();
                let dno = self.build_variable(&name, lineno, build_datatype(REFCURSOROID)?, true)?;
                if let Datum::Var(var) = &mut self.datums[dno as usize] {
                    var.cursor_explicit_expr = query;
                    var.cursor_explicit_argrow = args;
                    var.cursor_options = CURSOR_FAST_PLAN | options;
                }
                SV::None
            }
            "opt_scrollable" => SV::Int(match rhs.len() {
                0 => 0,
                2 => CURSOR_NO_SCROLL,
                _ => CURSOR_SCROLL,
            }),
            "decl_cursor_query" => SV::Expr(Some(self.read_sql_stmt()?)),
            "decl_cursor_args" if rhs.is_empty() => SV::None,
            "decl_cursor_args" => {
                let fields = take!(2).dnos().into_iter().map(|d| (self.refname(d), d)).collect();
                let lineno = self.lineno(at!(1));
                SV::Int(self.adddatum(Datum::Row(Row { refname: "(unnamed row)".into(), lineno, fields })))
            }
            "decl_cursor_arglist" if rhs.len() == 1 => SV::Dnos(vec![take!(1).int()]),
            "decl_cursor_arglist" => {
                let mut list = take!(1).dnos();
                list.push(take!(3).int());
                SV::Dnos(list)
            }
            "decl_cursor_arg" => {
                let SV::VarName(name, lineno) = take!(1) else { return Err("bad argument".into()) };
                let SV::Type(dtype) = take!(2) else { return Err("bad data type".into()) };
                SV::Int(self.build_variable(&name, lineno, dtype, true)?)
            }
            "decl_aliasitem" => {
                let tok = take!(1).tok();
                let found = match first {
                    "T_CWORD" => {
                        let idents = cword_of(&tok);
                        let found = match idents.as_slice() {
                            [a, b] => self.ns_lookup(self.ns_top, false, a, Some(b), None),
                            [a, b, c] => self.ns_lookup(self.ns_top, false, a, Some(b), Some(c)),
                            _ => None,
                        };
                        found.ok_or_else(|| format!("variable \"{}\" does not exist", idents.join(".")))?
                    }
                    _ => {
                        let name = word_of(&tok);
                        self.ns_lookup(self.ns_top, false, &name, None, None).ok_or_else(|| format!("variable \"{name}\" does not exist"))?
                    }
                };
                SV::NsItem(found.0)
            }
            "decl_varname" => {
                let name = word_of(&take!(1).tok());
                let lineno = self.lineno(at!(1));
                if self.ns_lookup(self.ns_top, true, &name, None, None).is_some() {
                    return Err(self.yyerror("duplicate declaration"));
                }
                SV::VarName(name, lineno)
            }
            "decl_const" | "decl_notnull" | "decl_collate" => SV::Bool(!rhs.is_empty()),
            "decl_datatype" => {
                let dtype = self.read_datatype(self.yychar)?;
                self.yychar = EMPTY;
                SV::Type(dtype)
            }
            "decl_defval" if first == "';'" => SV::Expr(None),
            "decl_defval" => SV::Expr(Some(self.read_sql_expression(t::SEMI, ";")?)),
            "proc_sect" if rhs.is_empty() => SV::Stmts(Vec::new()),
            "proc_sect" => {
                let mut list = take!(1).stmts();
                if let Some(s) = take!(2).stmt() {
                    list.push(s);
                }
                SV::Stmts(list)
            }
            "stmt_perform" => {
                let lineno = self.lineno(at!(1));
                self.push_back(t::K_PERFORM)?;
                let (mut expr, startloc, _) = self.read_sql_construct(t::SEMI, 0, 0, ";", PARSE_DEFAULT, false, false)?;
                expr.query = format!("SELECT{}", expr.query.get(7..).unwrap_or(""));
                self.check_sql_expr(&expr.query, expr.parse_mode)?;
                let _ = startloc;
                SV::Stmt(Some(Stmt::Perform { lineno, expr }))
            }
            "stmt_call" => {
                let lineno = self.lineno(at!(1));
                let is_call = first == "K_CALL";
                self.push_back(if is_call { t::K_CALL } else { t::K_DO })?;
                let expr = self.read_sql_stmt()?;
                SV::Stmt(Some(Stmt::Call { lineno, expr, is_call }))
            }
            "stmt_assign" => {
                let tok = take!(1).tok();
                let (dno, ident, _, idents) = wdatum(&tok);
                let mode = match if ident.is_some() { 1 } else { idents.len() } {
                    1 => PARSE_PLPGSQL_ASSIGN1,
                    2 => PARSE_PLPGSQL_ASSIGN2,
                    3 => PARSE_PLPGSQL_ASSIGN3,
                    _ => return Err("unexpected number of names".into()),
                };
                self.check_assignable(dno)?;
                let lineno = self.lineno(at!(1));
                self.push_back(t::T_DATUM)?;
                let (expr, _, _) = self.read_sql_construct(t::SEMI, 0, 0, ";", mode, false, true)?;
                SV::Stmt(Some(Stmt::Assign { lineno, varno: dno, expr }))
            }
            "stmt_getdiag" => {
                let lineno = self.lineno(at!(1));
                let is_stacked = take!(2).bool();
                let SV::DiagItems(items) = take!(4) else { return Err("bad diagnostics".into()) };
                for (_, kind) in &items {
                    match kind {
                        0 | 1 if is_stacked => return Err(format!("diagnostics item {} is not allowed in GET STACKED DIAGNOSTICS", GETDIAG_KINDS[*kind as usize])),
                        3..=12 if !is_stacked => return Err(format!("diagnostics item {} is not allowed in GET CURRENT DIAGNOSTICS", GETDIAG_KINDS[*kind as usize])),
                        _ => {}
                    }
                }
                SV::Stmt(Some(Stmt::GetDiag { lineno, is_stacked, diag_items: items }))
            }
            "getdiag_area_opt" => SV::Bool(first == "K_STACKED"),
            "getdiag_list" if rhs.len() == 1 => match take!(1) {
                SV::DiagItems(i) => SV::DiagItems(i),
                _ => SV::DiagItems(Vec::new()),
            },
            "getdiag_list" => {
                let SV::DiagItems(mut list) = take!(1) else { return Err("bad diagnostics".into()) };
                if let SV::DiagItems(mut one) = take!(3) {
                    list.append(&mut one);
                }
                SV::DiagItems(list)
            }
            "getdiag_list_item" => {
                let target = take!(1).int();
                let kind = take!(3).int();
                SV::DiagItems(vec![(target, kind)])
            }
            "getdiag_item" => {
                let tok = self.yylex()?;
                let kinds = [(t::K_ROW_COUNT, "row_count", 0), (t::K_PG_ROUTINE_OID, "pg_routine_oid", 1), (t::K_PG_CONTEXT, "pg_context", 2), (t::K_PG_EXCEPTION_DETAIL, "pg_exception_detail", 4), (t::K_PG_EXCEPTION_HINT, "pg_exception_hint", 5), (t::K_PG_EXCEPTION_CONTEXT, "pg_exception_context", 3), (t::K_COLUMN_NAME, "column_name", 7), (t::K_CONSTRAINT_NAME, "constraint_name", 8), (t::K_PG_DATATYPE_NAME, "pg_datatype_name", 9), (t::K_MESSAGE_TEXT, "message_text", 10), (t::K_TABLE_NAME, "table_name", 11), (t::K_SCHEMA_NAME, "schema_name", 12), (t::K_RETURNED_SQLSTATE, "returned_sqlstate", 6)];
                match kinds.iter().find(|(code, word, _)| self.tok_is_keyword(tok, *code, word)) {
                    Some((_, _, kind)) => SV::Int(*kind),
                    None => return Err(self.yyerror("unrecognized GET DIAGNOSTICS item")),
                }
            }
            "getdiag_target" | "cursor_variable" if first == "T_WORD" => return Err(word_is_not_variable(&word_of(&take!(1).tok()))),
            "getdiag_target" | "cursor_variable" | "for_variable" if first == "T_CWORD" => return Err(cword_is_not_variable(&cword_of(&take!(1).tok()))),
            "getdiag_target" => {
                let tok = take!(1).tok();
                let (dno, ..) = wdatum(&tok);
                let composite = matches!(self.datums[dno as usize], Datum::Row(_) | Datum::Rec(_));
                if composite || self.peek()? == LBRACKET {
                    return Err(format!("\"{}\" is not a scalar variable", name_of_datum(&tok)));
                }
                self.check_assignable(dno)?;
                SV::Int(dno)
            }
            "stmt_if" => {
                let lineno = self.lineno(at!(1));
                let cond = take!(2).expr().unwrap_or_else(empty_expr);
                let then_body = take!(3).stmts();
                let elsif_list = match take!(4) {
                    SV::Elsifs(e) => e,
                    _ => Vec::new(),
                };
                let else_body = take!(5).stmts();
                SV::Stmt(Some(Stmt::If { lineno, cond, then_body, elsif_list, else_body }))
            }
            "stmt_elsifs" if rhs.is_empty() => SV::Elsifs(Vec::new()),
            "stmt_elsifs" => {
                let mut list = match take!(1) {
                    SV::Elsifs(e) => e,
                    _ => Vec::new(),
                };
                let lineno = self.lineno(at!(2));
                list.push(IfElsif { lineno, cond: take!(3).expr().unwrap_or_else(empty_expr), stmts: take!(4).stmts() });
                SV::Elsifs(list)
            }
            "stmt_else" if rhs.is_empty() => SV::Stmts(Vec::new()),
            "stmt_else" => SV::Stmts(take!(2).stmts()),
            "stmt_case" => {
                let t_expr = take!(2).expr();
                let whens = match take!(3) {
                    SV::CaseWhens(w) => w,
                    _ => Vec::new(),
                };
                let else_stmts = match take!(4) {
                    SV::OptElse(e) => e,
                    _ => None,
                };
                SV::Stmt(Some(self.make_case(at!(1), t_expr, whens, else_stmts)?))
            }
            "opt_expr_until_when" => {
                let tok = self.yylex()?;
                let mut expr = None;
                if tok != t::K_WHEN {
                    self.push_back(tok)?;
                    expr = Some(self.read_sql_expression(t::K_WHEN, "WHEN")?);
                }
                self.push_back(t::K_WHEN)?;
                SV::Expr(expr)
            }
            "case_when_list" if rhs.len() == 1 => match take!(1) {
                SV::CaseWhen(w) => SV::CaseWhens(vec![w]),
                _ => SV::CaseWhens(Vec::new()),
            },
            "case_when_list" => {
                let mut list = match take!(1) {
                    SV::CaseWhens(w) => w,
                    _ => Vec::new(),
                };
                if let SV::CaseWhen(w) = take!(2) {
                    list.push(w);
                }
                SV::CaseWhens(list)
            }
            "case_when" => {
                let lineno = self.lineno(at!(1));
                SV::CaseWhen(CaseWhen { lineno, expr: take!(2).expr().unwrap_or_else(empty_expr), stmts: take!(3).stmts() })
            }
            "opt_case_else" if rhs.is_empty() => SV::OptElse(None),
            "opt_case_else" => SV::OptElse(Some(take!(2).stmts())),
            "stmt_loop" => {
                let label = take!(1).str();
                let SV::LoopBody { stmts, end_label, .. } = take!(3) else { return Err("bad loop".into()) };
                let lineno = self.lineno(at!(2));
                check_labels(&label, &end_label)?;
                self.ns_pop();
                SV::Stmt(Some(Stmt::Loop { lineno, label, body: stmts }))
            }
            "stmt_while" => {
                let label = take!(1).str();
                let cond = take!(3).expr().unwrap_or_else(empty_expr);
                let SV::LoopBody { stmts, end_label, .. } = take!(4) else { return Err("bad loop".into()) };
                let lineno = self.lineno(at!(2));
                check_labels(&label, &end_label)?;
                self.ns_pop();
                SV::Stmt(Some(Stmt::While { lineno, label, cond, body: stmts }))
            }
            "stmt_for" => {
                let label = take!(1).str();
                let control = take!(3).stmt();
                let SV::LoopBody { stmts, end_label, .. } = take!(4) else { return Err("bad loop".into()) };
                let lineno = self.lineno(at!(2));
                let stmt = control.map(|mut s| {
                    match &mut s {
                        Stmt::ForI { lineno: n, label: l, body, .. } => {
                            *n = lineno;
                            *l = label.clone();
                            *body = stmts;
                        }
                        Stmt::ForS { forq, .. } | Stmt::ForC { forq, .. } | Stmt::DynForS { forq, .. } => {
                            forq.lineno = lineno;
                            forq.label = label.clone();
                            forq.body = stmts;
                        }
                        _ => {}
                    }
                    s
                });
                check_labels(&label, &end_label)?;
                self.ns_pop();
                SV::Stmt(stmt)
            }
            "for_control" => {
                let SV::ForVar { name, lineno, scalar, row } = take!(1) else { return Err("bad loop variable".into()) };
                SV::Stmt(Some(self.for_control(name, lineno, scalar, row, at!(1))?))
            }
            "for_variable" if first == "T_DATUM" => {
                let tok = take!(1).tok();
                let name = name_of_datum(&tok);
                let lineno = self.lineno(at!(1));
                let (dno, ..) = wdatum(&tok);
                if matches!(self.datums[dno as usize], Datum::Row(_) | Datum::Rec(_)) {
                    SV::ForVar { name, lineno, scalar: None, row: Some(dno) }
                } else {
                    let tok = self.yylex()?;
                    self.push_back(tok)?;
                    let row = if tok == t::COMMA { Some(self.read_into_scalar_list(name.clone(), dno, at!(1))?) } else { None };
                    SV::ForVar { name, lineno, scalar: Some(dno), row }
                }
            }
            "for_variable" => {
                let name = word_of(&take!(1).tok());
                let lineno = self.lineno(at!(1));
                let tok = self.yylex()?;
                self.push_back(tok)?;
                if tok == t::COMMA {
                    return Err(word_is_not_variable(&name));
                }
                SV::ForVar { name, lineno, scalar: None, row: None }
            }
            "stmt_foreach_a" => {
                let label = take!(1).str();
                let lineno = self.lineno(at!(2));
                let SV::ForVar { scalar, row, .. } = take!(3) else { return Err("bad loop variable".into()) };
                let slice = take!(4).int();
                let expr = take!(7).expr().unwrap_or_else(empty_expr);
                let SV::LoopBody { stmts, end_label, .. } = take!(8) else { return Err("bad loop".into()) };
                let varno = match row.or(scalar) {
                    Some(dno) => {
                        self.check_assignable(dno)?;
                        dno
                    }
                    None => return Err("loop variable of FOREACH must be a known variable or list of variables".into()),
                };
                check_labels(&label, &end_label)?;
                self.ns_pop();
                SV::Stmt(Some(Stmt::ForeachA { lineno, label, varno, slice, expr, body: stmts }))
            }
            "foreach_slice" if rhs.is_empty() => SV::Int(0),
            "foreach_slice" => SV::Int(take!(2).int()),
            "stmt_exit" => {
                let is_exit = take!(1).bool();
                let lineno = self.lineno(at!(1));
                let label = take!(2).str();
                let cond = take!(3).expr();
                match &label {
                    Some(name) => {
                        let Some(item) = self.ns_lookup_label(name) else {
                            return Err(format!("there is no label \"{name}\" attached to any block or loop enclosing this statement"));
                        };
                        if self.ns[item].itemno != LABEL_LOOP && !is_exit {
                            return Err(format!("block label \"{name}\" cannot be used in CONTINUE"));
                        }
                    }
                    None if self.ns_find_nearest_loop().is_none() => return Err(if is_exit { "EXIT cannot be used outside a loop, unless it has a label" } else { "CONTINUE cannot be used outside a loop" }.into()),
                    None => {}
                }
                SV::Stmt(Some(Stmt::Exit { lineno, is_exit, label, cond }))
            }
            "exit_type" => SV::Bool(first == "K_EXIT"),
            "stmt_return" => {
                let tok = self.yylex()?;
                if tok == EOF {
                    return Err(self.yyerror("unexpected end of function definition"));
                }
                if self.tok_is_keyword(tok, t::K_NEXT, "next") {
                    SV::Stmt(Some(self.make_return_next_stmt(at!(1))?))
                } else if self.tok_is_keyword(tok, t::K_QUERY, "query") {
                    SV::Stmt(Some(self.make_return_query_stmt(at!(1))?))
                } else {
                    self.push_back(tok)?;
                    SV::Stmt(Some(self.make_return_stmt(at!(1))?))
                }
            }
            "stmt_raise" => SV::Stmt(Some(self.stmt_raise(at!(1))?)),
            "stmt_assert" => {
                let lineno = self.lineno(at!(1));
                let (cond, _, tok) = self.read_sql_construct(t::COMMA, t::SEMI, 0, ", or ;", PARSE_PLPGSQL_EXPR, true, true)?;
                let message = if tok == t::COMMA { Some(self.read_sql_expression(t::SEMI, ";")?) } else { None };
                SV::Stmt(Some(Stmt::Assert { lineno, cond, message }))
            }
            "loop_body" => SV::LoopBody { stmts: take!(1).stmts(), end_label: take!(4).str() },
            "stmt_execsql" => {
                let tok = take!(1).tok();
                let (code, word) = match first {
                    "K_IMPORT" => (t::K_IMPORT, None),
                    "K_INSERT" => (t::K_INSERT, None),
                    "K_MERGE" => (t::K_MERGE, None),
                    "T_WORD" => {
                        let next = self.yylex()?;
                        self.push_back(next)?;
                        if matches!(next, t::EQUALS | t::COLON_EQUALS | LBRACKET | DOT) {
                            return Err(word_is_not_variable(&word_of(&tok)));
                        }
                        (t::T_WORD, Some(word_of(&tok)))
                    }
                    _ => {
                        let next = self.yylex()?;
                        self.push_back(next)?;
                        if matches!(next, t::EQUALS | t::COLON_EQUALS | LBRACKET | DOT) {
                            return Err(cword_is_not_variable(&cword_of(&tok)));
                        }
                        (t::T_CWORD, None)
                    }
                };
                SV::Stmt(Some(self.make_execsql_stmt(code, at!(1), word)?))
            }
            "stmt_dynexecute" => SV::Stmt(Some(self.stmt_dynexecute(at!(1))?)),
            "stmt_open" => {
                let curvar = take!(2).int();
                SV::Stmt(Some(self.stmt_open(at!(1), curvar)?))
            }
            "stmt_fetch" => {
                let SV::Fetch(mut fetch) = take!(2) else { return Err("bad fetch".into()) };
                let curvar = take!(3).int();
                let target = self.read_into_target(None)?;
                if self.yylex()? != t::SEMI {
                    return Err(self.yyerror("syntax error"));
                }
                if fetch.returns_multiple_rows {
                    return Err("FETCH statement cannot return multiple rows".into());
                }
                fetch.lineno = self.lineno(at!(1));
                fetch.target = target;
                fetch.curvar = curvar;
                fetch.is_move = false;
                SV::Stmt(Some(Stmt::Fetch(fetch)))
            }
            "stmt_move" => {
                let SV::Fetch(mut fetch) = take!(2) else { return Err("bad move".into()) };
                fetch.lineno = self.lineno(at!(1));
                fetch.curvar = take!(3).int();
                fetch.is_move = true;
                SV::Stmt(Some(Stmt::Fetch(fetch)))
            }
            "opt_fetch_direction" => SV::Fetch(self.read_fetch_direction()?),
            "stmt_close" => {
                let lineno = self.lineno(at!(1));
                SV::Stmt(Some(Stmt::Close { lineno, curvar: take!(2).int() }))
            }
            "stmt_null" => SV::Stmt(None),
            "stmt_commit" | "stmt_rollback" => {
                let lineno = self.lineno(at!(1));
                let chain = take!(2).bool();
                SV::Stmt(Some(if lhs == "stmt_commit" { Stmt::Commit { lineno, chain } } else { Stmt::Rollback { lineno, chain } }))
            }
            "opt_transaction_chain" => SV::Bool(rhs.len() == 2),
            "cursor_variable" => {
                let tok = take!(1).tok();
                let (dno, ..) = wdatum(&tok);
                let is_var = matches!(&self.datums[dno as usize], Datum::Var(v) if v.promise == 0);
                if !is_var || self.peek()? == LBRACKET {
                    return Err("cursor variable must be a simple variable".into());
                }
                if let Datum::Var(var) = &self.datums[dno as usize] {
                    if var.datatype.as_ref().map_or(0, |t| t.typoid) != REFCURSOROID {
                        return Err(format!("variable \"{}\" must be of type cursor or refcursor", var.refname));
                    }
                }
                SV::Int(dno)
            }
            "exception_sect" if rhs.is_empty() => SV::ExcBlock(None),
            "$@2" => {
                let lineno = self.lineno(l[base - 1]);
                let sqlstate = self.build_variable("sqlstate", lineno, build_datatype(TEXTOID)?, true)?;
                let sqlerrm = self.build_variable("sqlerrm", lineno, build_datatype(TEXTOID)?, true)?;
                for dno in [sqlstate, sqlerrm] {
                    if let Datum::Var(var) = &mut self.datums[dno as usize] {
                        var.isconst = true;
                    }
                }
                SV::ExcBlock(Some(ExceptionBlock { sqlstate_varno: sqlstate, sqlerrm_varno: sqlerrm, exc_list: Vec::new() }))
            }
            "exception_sect" => {
                let SV::ExcBlock(Some(mut block)) = take!(2) else { return Err("bad exception block".into()) };
                if let SV::Exceptions(list) = take!(3) {
                    block.exc_list = list;
                }
                SV::ExcBlock(Some(block))
            }
            "proc_exceptions" if rhs.len() == 1 => match take!(1) {
                SV::Exception(e) => SV::Exceptions(vec![e]),
                _ => SV::Exceptions(Vec::new()),
            },
            "proc_exceptions" => {
                let mut list = match take!(1) {
                    SV::Exceptions(l) => l,
                    _ => Vec::new(),
                };
                if let SV::Exception(e) = take!(2) {
                    list.push(e);
                }
                SV::Exceptions(list)
            }
            "proc_exception" => {
                let conditions = match take!(2) {
                    SV::Conditions(c) => c,
                    _ => Vec::new(),
                };
                SV::Exception(Exception { conditions, action: take!(4).stmts() })
            }
            "proc_conditions" if rhs.len() == 1 => take!(1),
            "proc_conditions" => {
                let mut list = match take!(1) {
                    SV::Conditions(c) => c,
                    _ => Vec::new(),
                };
                if let SV::Conditions(mut more) = take!(3) {
                    list.append(&mut more);
                }
                SV::Conditions(list)
            }
            "proc_condition" => {
                let name = take!(1).str().unwrap_or_default();
                if name != "sqlstate" {
                    SV::Conditions(parse_err_condition(&name)?)
                } else {
                    if self.yylex()? != t::SCONST {
                        return Err(self.yyerror("syntax error"));
                    }
                    let state = self.cur.str();
                    if !valid_sqlstate(&state) {
                        return Err(self.yyerror("invalid SQLSTATE code"));
                    }
                    SV::Conditions(vec![Condition { condname: state }])
                }
            }
            "expr_until_semi" => SV::Expr(Some(self.read_sql_expression(t::SEMI, ";")?)),
            "expr_until_then" => SV::Expr(Some(self.read_sql_expression(t::K_THEN, "THEN")?)),
            "expr_until_loop" => SV::Expr(Some(self.read_sql_expression(t::K_LOOP, "LOOP")?)),
            "opt_block_label" | "opt_loop_label" => {
                let kind = if lhs == "opt_block_label" { LABEL_BLOCK } else { LABEL_LOOP };
                let label = if rhs.is_empty() { None } else { take!(2).str() };
                self.ns_push(label.as_deref(), kind);
                SV::Str(label)
            }
            "opt_label" if rhs.is_empty() => SV::Str(None),
            "opt_label" => SV::Str(take!(1).str()),
            "opt_exitcond" if first == "';'" => SV::Expr(None),
            "opt_exitcond" => SV::Expr(take!(2).expr()),
            "any_identifier" if first == "T_DATUM" => {
                let tok = take!(1).tok();
                match wdatum(&tok).1 {
                    Some(ident) => SV::Str(Some(ident)),
                    None => return Err(self.yyerror("syntax error")),
                }
            }
            "any_identifier" => SV::Str(Some(word_of(&take!(1).tok()))),
            _ => return Ok(None),
        }))
    }

    /// `tok_is_keyword`: the keyword, or a variable spelled the same, which the scanner preferred.
    pub fn tok_is_keyword(&self, token: Code, kw: Code, word: &str) -> bool {
        if token == kw {
            return true;
        }
        if token == t::T_DATUM {
            if let Val::Datum { ident: Some(ident), quoted: false, .. } = &self.cur.val {
                return ident == word;
            }
        }
        false
    }

    /// `current_token_is_not_variable`.
    fn current_token_is_not_variable(&self, tok: Code) -> String {
        match tok {
            t::T_WORD => word_is_not_variable(&word_of(&self.cur)),
            t::T_CWORD => cword_is_not_variable(&cword_of(&self.cur)),
            _ => self.yyerror("syntax error"),
        }
    }

    fn read_sql_expression(&mut self, until: Code, expected: &str) -> R<Expr> {
        Ok(self.read_sql_construct(until, 0, 0, expected, PARSE_PLPGSQL_EXPR, true, true)?.0)
    }

    fn read_sql_expression2(&mut self, until: Code, until2: Code, expected: &str) -> R<(Expr, Code)> {
        let (expr, _, end) = self.read_sql_construct(until, until2, 0, expected, PARSE_PLPGSQL_EXPR, true, true)?;
        Ok((expr, end))
    }

    fn read_sql_stmt(&mut self) -> R<Expr> {
        Ok(self.read_sql_construct(t::SEMI, 0, 0, ";", PARSE_DEFAULT, false, true)?.0)
    }

    /// `read_sql_construct`: the text up to a terminator outside parentheses, with where it starts
    /// and which terminator ended it.
    #[allow(clippy::too_many_arguments)]
    fn read_sql_construct(&mut self, until: Code, until2: Code, until3: Code, expected: &str, mode: i32, isexpression: bool, valid_sql: bool) -> R<(Expr, i32, Code)> {
        let saved = self.set_lookup(Lookup::Expr);
        let mut start = -1;
        let mut end = -1;
        let mut paren = 0;
        let tok = loop {
            let tok = self.yylex()?;
            if start < 0 {
                start = self.cur.loc;
            }
            if paren == 0 && (tok == until || tok == until2 || tok == until3) {
                break tok;
            }
            if tok == t::LPAREN || tok == LBRACKET {
                paren += 1;
            } else if tok == t::RPAREN || tok == RBRACKET {
                paren -= 1;
                if paren < 0 {
                    return Err(self.yyerror("mismatched parentheses"));
                }
            }
            if tok == EOF || tok == t::SEMI {
                if paren != 0 {
                    return Err(self.yyerror("mismatched parentheses"));
                }
                return Err(format!("missing \"{expected}\" at end of SQL {}", if isexpression { "expression" } else { "statement" }));
            }
            end = self.cur.loc + self.yyleng;
        };
        self.lookup = saved;
        if start >= end {
            return Err(self.yyerror(if isexpression { "missing expression" } else { "missing SQL statement" }));
        }
        let query = self.text(start, end);
        let expr = Expr { query, parse_mode: mode };
        if valid_sql {
            self.check_sql_expr(&expr.query, mode)?;
        }
        Ok((expr, start, tok))
    }

    fn text(&self, start: i32, end: i32) -> String {
        let b = self.src.as_bytes();
        let (s, e) = (start.max(0) as usize, (end.max(0) as usize).min(b.len()));
        String::from_utf8_lossy(&b[s.min(e)..e]).into_owned()
    }

    /// `check_sql_expr`: Postgres's parser must read the text in the mode it is for.
    fn check_sql_expr(&self, query: &str, mode: i32) -> R<()> {
        crate::postgres::parse_in_mode(query, mode).map(|_| ())
    }

    /// `read_datatype`: a declaration's type, up to what follows it.
    fn read_datatype(&mut self, lookahead: Code) -> R<Type> {
        let mut tok = if lookahead == EMPTY { self.yylex()? } else { lookahead };
        let start = self.cur.loc;
        let mut result: Option<Type> = None;
        if tok == t::T_WORD || is_unreserved_keyword(tok) || tok == t::T_CWORD {
            let compound = tok == t::T_CWORD;
            let name = if compound { cword_of(&self.cur).join(".") } else { word_of(&self.cur) };
            tok = self.yylex()?;
            if tok == PERCENT {
                tok = self.yylex()?;
                if self.tok_is_keyword(tok, t::K_TYPE, "type") {
                    result = Some(pct_type(format!("{name}%TYPE")));
                } else if self.tok_is_keyword(tok, t::K_ROWTYPE, "rowtype") {
                    result = Some(pct_type(format!("{name}%rowtype")));
                }
            }
        }
        if let Some(mut result) = result {
            let mut is_array = false;
            tok = self.yylex()?;
            if self.tok_is_keyword(tok, t::K_ARRAY, "array") {
                is_array = true;
                tok = self.yylex()?;
            }
            while tok == LBRACKET {
                is_array = true;
                tok = self.yylex()?;
                if tok == t::ICONST {
                    tok = self.yylex()?;
                }
                if tok != RBRACKET {
                    return Err(self.yyerror("syntax error, expected \"]\""));
                }
                tok = self.yylex()?;
            }
            self.push_back(tok)?;
            if is_array {
                let array = super::compile::type_entry(result.typoid)?.3;
                if array == 0 {
                    return Err(format!("could not find array type for data type {}", format_type(result.typoid)?));
                }
                result = build_datatype(array)?;
            }
            return Ok(result);
        }
        let mut paren = 0;
        while tok != t::SEMI {
            if tok == EOF {
                return Err(self.yyerror(if paren != 0 { "mismatched parentheses" } else { "incomplete data type declaration" }));
            }
            if matches!(tok, t::K_COLLATE | t::K_NOT | t::EQUALS | t::COLON_EQUALS | t::K_DEFAULT) {
                break;
            }
            if (tok == t::COMMA || tok == t::RPAREN) && paren == 0 {
                break;
            }
            if tok == t::LPAREN {
                paren += 1;
            } else if tok == t::RPAREN {
                paren -= 1;
            }
            tok = self.yylex()?;
        }
        let name = self.text(start, self.cur.loc);
        if name.is_empty() {
            return Err(self.yyerror("missing data type declaration"));
        }
        let result = parse_datatype(&name)?;
        self.push_back(tok)?;
        Ok(result)
    }

    /// `make_execsql_stmt`: a SQL statement PL/pgSQL runs as written, its INTO taken out.
    fn make_execsql_stmt(&mut self, first: Code, location: i32, word: Option<String>) -> R<Stmt> {
        let saved = self.set_lookup(Lookup::Expr);
        let mut tok = first;
        let mut target = None;
        let mut have_into = false;
        let mut have_strict = false;
        let (mut into_start, mut into_end) = (-1, -1);
        let (mut paren_depth, mut begin_depth) = (0, 0);
        let mut in_routine = false;
        let mut tokens = [0u8; 4];
        if tok == t::T_WORD && word.as_deref() == Some("create") {
            tokens[0] = b'c';
        }
        let mut token_count = 1;
        loop {
            let prev = tok;
            tok = self.yylex()?;
            if have_into && into_end < 0 {
                into_end = self.cur.loc;
            }
            if tokens[0] == b'c' && token_count < tokens.len() {
                let word = if tok == t::T_WORD { word_of(&self.cur) } else { String::new() };
                if tok == t::K_OR {
                    tokens[token_count] = b'o';
                } else if word == "replace" {
                    tokens[token_count] = b'r';
                } else if word == "function" || word == "procedure" {
                    tokens[token_count] = b'f';
                }
                if tokens[1] == b'f' || (tokens[1] == b'o' && tokens[2] == b'r' && tokens[3] == b'f') {
                    in_routine = true;
                }
                token_count += 1;
            }
            if tok == t::LPAREN {
                paren_depth += 1;
            } else if tok == t::RPAREN && paren_depth > 0 {
                paren_depth -= 1;
            }
            if in_routine && paren_depth == 0 {
                if tok == t::K_BEGIN || tok == t::K_CASE {
                    begin_depth += 1;
                } else if tok == t::K_END && begin_depth > 0 {
                    begin_depth -= 1;
                }
            }
            if tok == t::SEMI && paren_depth == 0 && begin_depth == 0 {
                break;
            }
            if tok == EOF {
                return Err(self.yyerror("unexpected end of function definition"));
            }
            if tok == t::K_INTO {
                if prev == t::K_INSERT || prev == t::K_MERGE || first == t::K_IMPORT {
                    continue;
                }
                if have_into {
                    return Err(self.yyerror("INTO specified more than once"));
                }
                have_into = true;
                into_start = self.cur.loc;
                self.lookup = Lookup::Normal;
                target = self.read_into_target(Some(&mut have_strict))?;
                self.lookup = Lookup::Expr;
            }
        }
        self.lookup = saved;
        let mut query = if have_into {
            let mut q = self.text(location, into_start);
            q.push_str(&" ".repeat((into_end - into_start).max(0) as usize));
            q.push_str(&self.text(into_end, self.cur.loc));
            q
        } else {
            self.text(location, self.cur.loc)
        };
        while query.ends_with([' ', '\t', '\n', '\r', '\u{b}', '\u{c}']) {
            query.pop();
        }
        self.check_sql_expr(&query, PARSE_DEFAULT)?;
        Ok(Stmt::ExecSql { lineno: self.lineno(location), sqlstmt: Expr { query, parse_mode: PARSE_DEFAULT }, into: have_into, strict: have_strict, target })
    }

    /// `read_fetch_direction`: FETCH or MOVE's direction, through FROM or IN.
    fn read_fetch_direction(&mut self) -> R<Fetch> {
        let mut fetch = Fetch { lineno: 0, target: None, curvar: 0, direction: FETCH_FORWARD, how_many: 1, expr: None, is_move: false, returns_multiple_rows: false };
        let mut check_from = true;
        let tok = self.yylex()?;
        if tok == EOF {
            return Err(self.yyerror("unexpected end of function definition"));
        }
        if self.tok_is_keyword(tok, t::K_NEXT, "next") {
        } else if self.tok_is_keyword(tok, t::K_PRIOR, "prior") {
            fetch.direction = FETCH_BACKWARD;
        } else if self.tok_is_keyword(tok, t::K_FIRST, "first") {
            fetch.direction = FETCH_ABSOLUTE;
        } else if self.tok_is_keyword(tok, t::K_LAST, "last") {
            fetch.direction = FETCH_ABSOLUTE;
            fetch.how_many = -1;
        } else if self.tok_is_keyword(tok, t::K_ABSOLUTE, "absolute") {
            fetch.direction = FETCH_ABSOLUTE;
            fetch.expr = Some(self.read_sql_expression2(t::K_FROM, t::K_IN, "FROM or IN")?.0);
            check_from = false;
        } else if self.tok_is_keyword(tok, t::K_RELATIVE, "relative") {
            fetch.direction = FETCH_RELATIVE;
            fetch.expr = Some(self.read_sql_expression2(t::K_FROM, t::K_IN, "FROM or IN")?.0);
            check_from = false;
        } else if self.tok_is_keyword(tok, t::K_ALL, "all") {
            fetch.how_many = FETCH_ALL;
            fetch.returns_multiple_rows = true;
        } else if self.tok_is_keyword(tok, t::K_FORWARD, "forward") {
            check_from = self.complete_direction(&mut fetch)?;
        } else if self.tok_is_keyword(tok, t::K_BACKWARD, "backward") {
            fetch.direction = FETCH_BACKWARD;
            check_from = self.complete_direction(&mut fetch)?;
        } else if tok == t::K_FROM || tok == t::K_IN {
            check_from = false;
        } else if tok == t::T_DATUM {
            self.push_back(tok)?;
            check_from = false;
        } else {
            self.push_back(tok)?;
            fetch.expr = Some(self.read_sql_expression2(t::K_FROM, t::K_IN, "FROM or IN")?.0);
            fetch.returns_multiple_rows = true;
            check_from = false;
        }
        if check_from {
            let tok = self.yylex()?;
            if tok != t::K_FROM && tok != t::K_IN {
                return Err(self.yyerror("expected FROM or IN"));
            }
        }
        Ok(fetch)
    }

    /// `complete_direction`: whether FROM or IN is still to come.
    fn complete_direction(&mut self, fetch: &mut Fetch) -> R<bool> {
        let tok = self.yylex()?;
        if tok == EOF {
            return Err(self.yyerror("unexpected end of function definition"));
        }
        if tok == t::K_FROM || tok == t::K_IN {
            return Ok(false);
        }
        if tok == t::K_ALL {
            fetch.how_many = FETCH_ALL;
            fetch.returns_multiple_rows = true;
            return Ok(true);
        }
        self.push_back(tok)?;
        fetch.expr = Some(self.read_sql_expression2(t::K_FROM, t::K_IN, "FROM or IN")?.0);
        fetch.returns_multiple_rows = true;
        Ok(false)
    }

    /// A RETURN or RETURN NEXT of a variable alone, which is kept as its datum.
    fn simple_return(&mut self) -> R<(Option<Expr>, i32)> {
        let tok = self.yylex()?;
        if tok == t::T_DATUM && self.peek()? == t::SEMI {
            let (dno, ..) = wdatum(&self.cur);
            if !matches!(self.datums[dno as usize], Datum::RecField(_)) {
                self.yylex()?;
                return Ok((None, dno));
            }
        }
        self.push_back(tok)?;
        Ok((Some(self.read_sql_expression(t::SEMI, ";")?), -1))
    }

    fn make_return_stmt(&mut self, location: i32) -> R<Stmt> {
        let lineno = self.lineno(location);
        let (mut expr, mut retvarno) = (None, -1);
        if self.func.retset {
            if self.yylex()? != t::SEMI {
                return Err("RETURN cannot have a parameter in function returning set".into());
            }
        } else if self.func.rettype == VOIDOID {
            if self.yylex()? != t::SEMI {
                return Err(if self.func.procedure { "RETURN cannot have a parameter in a procedure" } else { "RETURN cannot have a parameter in function returning void" }.into());
            }
        } else if self.func.out_param_varno >= 0 {
            if self.yylex()? != t::SEMI {
                return Err("RETURN cannot have a parameter in function with OUT parameters".into());
            }
            retvarno = self.func.out_param_varno;
        } else {
            (expr, retvarno) = self.simple_return()?;
        }
        Ok(Stmt::Return { lineno, expr, retvarno })
    }

    fn make_return_next_stmt(&mut self, location: i32) -> R<Stmt> {
        if !self.func.retset {
            return Err("cannot use RETURN NEXT in a non-SETOF function".into());
        }
        let lineno = self.lineno(location);
        let (expr, retvarno) = if self.func.out_param_varno >= 0 {
            if self.yylex()? != t::SEMI {
                return Err("RETURN NEXT cannot have a parameter in function with OUT parameters".into());
            }
            (None, self.func.out_param_varno)
        } else {
            self.simple_return()?
        };
        Ok(Stmt::ReturnNext { lineno, expr, retvarno })
    }

    fn make_return_query_stmt(&mut self, location: i32) -> R<Stmt> {
        if !self.func.retset {
            return Err("cannot use RETURN QUERY in a non-SETOF function".into());
        }
        let lineno = self.lineno(location);
        let tok = self.yylex()?;
        if tok != t::K_EXECUTE {
            self.push_back(tok)?;
            let query = self.read_sql_stmt()?;
            return Ok(Stmt::ReturnQuery { lineno, query: Some(query), dynquery: None, params: Vec::new() });
        }
        let (dynquery, mut term) = self.read_sql_expression2(t::SEMI, t::K_USING, "; or USING")?;
        let mut params = Vec::new();
        if term == t::K_USING {
            loop {
                let (expr, end) = self.read_sql_expression2(t::COMMA, t::SEMI, ", or ;")?;
                params.push(expr);
                term = end;
                if term != t::COMMA {
                    break;
                }
            }
        }
        Ok(Stmt::ReturnQuery { lineno, query: None, dynquery: Some(dynquery), params })
    }

    /// `check_assignable`.
    fn check_assignable(&self, dno: i32) -> R<()> {
        match &self.datums[dno as usize] {
            Datum::Var(v) if v.isconst => Err(format!("variable \"{}\" is declared CONSTANT", v.refname)),
            Datum::Rec(r) if r.isconst => Err(format!("variable \"{}\" is declared CONSTANT", r.refname)),
            Datum::RecField(f) => self.check_assignable(f.recparentno),
            _ => Ok(()),
        }
    }

    /// `read_into_target`: the variable, or the list of them, after INTO.
    fn read_into_target(&mut self, strict: Option<&mut bool>) -> R<Option<i32>> {
        let mut tok = self.yylex()?;
        if let Some(strict) = strict {
            *strict = false;
            if tok == t::K_STRICT {
                *strict = true;
                tok = self.yylex()?;
            }
        }
        if tok != t::T_DATUM {
            return Err(self.current_token_is_not_variable(tok));
        }
        let (dno, ..) = wdatum(&self.cur);
        if matches!(self.datums[dno as usize], Datum::Row(_) | Datum::Rec(_)) {
            self.check_assignable(dno)?;
            let next = self.yylex()?;
            if next == t::COMMA {
                return Err("record variable cannot be part of multiple-item INTO list".into());
            }
            self.push_back(next)?;
            Ok(Some(dno))
        } else {
            let name = name_of_datum(&self.cur);
            let loc = self.cur.loc;
            Ok(Some(self.read_into_scalar_list(name, dno, loc)?))
        }
    }

    /// `read_into_scalar_list`: a row of the comma-separated scalar variables.
    fn read_into_scalar_list(&mut self, name: String, dno: i32, location: i32) -> R<i32> {
        self.check_assignable(dno)?;
        let mut fields = vec![(name, dno)];
        let mut tok;
        loop {
            tok = self.yylex()?;
            if tok != t::COMMA {
                break;
            }
            if fields.len() >= 1024 {
                return Err("too many INTO variables specified".into());
            }
            tok = self.yylex()?;
            if tok != t::T_DATUM {
                return Err(self.current_token_is_not_variable(tok));
            }
            let (dno, ..) = wdatum(&self.cur);
            self.check_assignable(dno)?;
            if matches!(self.datums[dno as usize], Datum::Row(_) | Datum::Rec(_)) {
                return Err(format!("\"{}\" is not a scalar variable", name_of_datum(&self.cur)));
            }
            fields.push((name_of_datum(&self.cur), dno));
        }
        self.push_back(tok)?;
        let lineno = self.lineno(location);
        Ok(self.adddatum(Datum::Row(Row { refname: "(unnamed row)".into(), lineno, fields })))
    }

    /// `make_scalar_list1`.
    fn make_scalar_list1(&mut self, name: String, dno: i32, lineno: i32) -> R<i32> {
        self.check_assignable(dno)?;
        Ok(self.adddatum(Datum::Row(Row { refname: "(unnamed row)".into(), lineno, fields: vec![(name, dno)] })))
    }

    /// The loop variable of a loop over rows: a record or row, or a single scalar made a row.
    fn row_target(&mut self, name: String, lineno: i32, scalar: Option<i32>, row: Option<i32>, message: &str) -> R<i32> {
        match (row, scalar) {
            (Some(row), _) => {
                self.check_assignable(row)?;
                Ok(row)
            }
            (None, Some(scalar)) => self.make_scalar_list1(name, scalar, lineno),
            _ => Err(message.to_string()),
        }
    }

    /// `for_control`: an integer loop, a loop over a query, a cursor or a dynamic query.
    fn for_control(&mut self, name: String, lineno: i32, scalar: Option<i32>, row: Option<i32>, _location: i32) -> R<Stmt> {
        const ROWS: &str = "loop variable of loop over rows must be a record variable or list of scalar variables";
        let mut tok = self.yylex()?;
        let forq = |var| Forq { lineno: 0, label: None, var, body: Vec::new() };
        if tok == t::K_EXECUTE {
            let (query, mut term) = self.read_sql_expression2(t::K_LOOP, t::K_USING, "LOOP or USING")?;
            let var = self.row_target(name, lineno, scalar, row, ROWS)?;
            let mut params = Vec::new();
            if term == t::K_USING {
                loop {
                    let (expr, end) = self.read_sql_expression2(t::COMMA, t::K_LOOP, ", or LOOP")?;
                    params.push(expr);
                    term = end;
                    if term != t::COMMA {
                        break;
                    }
                }
            }
            return Ok(Stmt::DynForS { forq: forq(var), query, params });
        }
        if tok == t::T_DATUM {
            let (dno, ..) = wdatum(&self.cur);
            if let Datum::Var(cursor) = &self.datums[dno as usize] {
                if cursor.promise == 0 && cursor.datatype.as_ref().is_some_and(|t| t.typoid == REFCURSOROID) {
                    if scalar.is_some() && row.is_some() {
                        return Err("cursor FOR loop must have only one target variable".into());
                    }
                    if cursor.cursor_explicit_expr.is_none() {
                        return Err("cursor FOR loop must use a bound cursor variable".into());
                    }
                    let argquery = self.read_cursor_args(dno, t::K_LOOP)?;
                    let var = self.build_record(&name, lineno, true);
                    return Ok(Stmt::ForC { forq: forq(var), curvar: dno, argquery });
                }
            }
        }
        let reverse = self.tok_is_keyword(tok, t::K_REVERSE, "reverse");
        if !reverse {
            self.push_back(tok)?;
        }
        let (mut expr1, _, end) = self.read_sql_construct(t::DOT_DOT, t::K_LOOP, 0, "LOOP", PARSE_DEFAULT, true, false)?;
        tok = end;
        if tok == t::DOT_DOT {
            expr1.parse_mode = PARSE_PLPGSQL_EXPR;
            self.check_sql_expr(&expr1.query, expr1.parse_mode)?;
            let (expr2, end) = self.read_sql_expression2(t::K_LOOP, t::K_BY, "LOOP")?;
            let step = if end == t::K_BY { Some(self.read_sql_expression(t::K_LOOP, "LOOP")?) } else { None };
            if scalar.is_some() && row.is_some() {
                return Err("integer FOR loop must have only one target variable".into());
            }
            let var = self.build_variable(&name, lineno, build_datatype(INT4OID)?, true)?;
            return Ok(Stmt::ForI { lineno: 0, label: None, var, lower: expr1, upper: expr2, step, reverse, body: Vec::new() });
        }
        if reverse {
            return Err("cannot specify REVERSE in query FOR loop".into());
        }
        self.check_sql_expr(&expr1.query, expr1.parse_mode)?;
        let var = self.row_target(name, lineno, scalar, row, ROWS)?;
        Ok(Stmt::ForS { forq: forq(var), query: expr1 })
    }

    /// `read_cursor_args`: a bound cursor's arguments, as one SELECT list in its parameters' order.
    fn read_cursor_args(&mut self, cursor: i32, until: Code) -> R<Option<Expr>> {
        let (refname, argrow) = match &self.datums[cursor as usize] {
            Datum::Var(v) => (v.refname.clone(), v.cursor_explicit_argrow),
            _ => (String::new(), -1),
        };
        let tok = self.yylex()?;
        if argrow < 0 {
            if tok == t::LPAREN {
                return Err(format!("cursor \"{refname}\" has no arguments"));
            }
            if tok != until {
                return Err(self.yyerror("syntax error"));
            }
            return Ok(None);
        }
        if tok != t::LPAREN {
            return Err(format!("cursor \"{refname}\" has arguments"));
        }
        let names: Vec<String> = match &self.datums[argrow as usize] {
            Datum::Row(r) => r.fields.iter().map(|f| f.0.clone()).collect(),
            _ => Vec::new(),
        };
        let n = names.len();
        let mut argv: Vec<Option<String>> = vec![None; n];
        let mut any_named = false;
        for argc in 0..n {
            let (tok1, tok2, _) = self.peek2()?;
            let argpos = if tok1 == t::IDENT && (tok2 == t::COLON_EQUALS || tok2 == t::EQUALS_GREATER) {
                let saved = self.set_lookup(Lookup::Declare);
                self.yylex()?;
                let argname = self.cur.str();
                self.lookup = saved;
                let Some(pos) = names.iter().position(|f| *f == argname) else {
                    return Err(format!("cursor \"{refname}\" has no argument named \"{argname}\""));
                };
                let op = self.yylex()?;
                if op != t::COLON_EQUALS && op != t::EQUALS_GREATER {
                    return Err(self.yyerror("syntax error"));
                }
                any_named = true;
                pos
            } else {
                argc
            };
            if argv[argpos].is_some() {
                return Err(format!("value for parameter \"{}\" of cursor \"{refname}\" specified more than once", names[argpos]));
            }
            let (item, _, end) = self.read_sql_construct(t::COMMA, t::RPAREN, 0, ",\" or \")", PARSE_PLPGSQL_EXPR, true, true)?;
            argv[argpos] = Some(item.query);
            if end == t::RPAREN && argc != n - 1 {
                return Err(format!("not enough arguments for cursor \"{refname}\""));
            }
            if end == t::COMMA && argc == n - 1 {
                return Err(format!("too many arguments for cursor \"{refname}\""));
            }
        }
        let mut query = String::new();
        for (i, arg) in argv.iter().enumerate() {
            query.push_str(arg.as_deref().unwrap_or(""));
            if any_named {
                query.push_str(&format!(" AS {}", quote_identifier(&names[i])));
            }
            if i + 1 < n {
                query.push_str(", ");
            }
        }
        if self.yylex()? != until {
            return Err(self.yyerror("syntax error"));
        }
        Ok(Some(Expr { query, parse_mode: PARSE_PLPGSQL_EXPR }))
    }

    /// `read_raise_options`: RAISE's USING list.
    fn read_raise_options(&mut self) -> R<Vec<(i32, Expr)>> {
        let mut out = Vec::new();
        loop {
            let tok = self.yylex()?;
            if tok == EOF {
                return Err(self.yyerror("unexpected end of function definition"));
            }
            let kinds = [(t::K_ERRCODE, "errcode"), (t::K_MESSAGE, "message"), (t::K_DETAIL, "detail"), (t::K_HINT, "hint"), (t::K_COLUMN, "column"), (t::K_CONSTRAINT, "constraint"), (t::K_DATATYPE, "datatype"), (t::K_TABLE, "table"), (t::K_SCHEMA, "schema")];
            let Some(kind) = kinds.iter().position(|(code, word)| self.tok_is_keyword(tok, *code, word)) else {
                return Err(self.yyerror("unrecognized RAISE statement option"));
            };
            let tok = self.yylex()?;
            if tok != t::EQUALS && tok != t::COLON_EQUALS {
                return Err(self.yyerror("syntax error, expected \"=\""));
            }
            let (expr, end) = self.read_sql_expression2(t::COMMA, t::SEMI, ", or ;")?;
            out.push((kind as i32, expr));
            if end == t::SEMI {
                return Ok(out);
            }
        }
    }

    fn stmt_raise(&mut self, location: i32) -> R<Stmt> {
        let lineno = self.lineno(location);
        let mut elog_level = ERROR;
        let (mut condname, mut message) = (None, None);
        let mut params = Vec::new();
        let mut options = Vec::new();
        let mut tok = self.yylex()?;
        if tok == EOF {
            return Err(self.yyerror("unexpected end of function definition"));
        }
        if tok != t::SEMI {
            let levels = [(t::K_EXCEPTION, "exception", ERROR), (t::K_WARNING, "warning", WARNING), (t::K_NOTICE, "notice", NOTICE), (t::K_INFO, "info", INFO), (t::K_LOG, "log", LOG), (t::K_DEBUG, "debug", DEBUG1)];
            if let Some(&(_, _, level)) = levels.iter().find(|(code, word, _)| self.tok_is_keyword(tok, *code, word)) {
                elog_level = level;
                tok = self.yylex()?;
            }
            if tok == EOF {
                return Err(self.yyerror("unexpected end of function definition"));
            }
            if tok == t::SCONST {
                message = Some(self.cur.str());
                tok = self.yylex()?;
                if tok != t::COMMA && tok != t::SEMI && tok != t::K_USING {
                    return Err(self.yyerror("syntax error"));
                }
                while tok == t::COMMA {
                    let (expr, _, end) = self.read_sql_construct(t::COMMA, t::SEMI, t::K_USING, ", or ; or USING", PARSE_PLPGSQL_EXPR, true, true)?;
                    params.push(expr);
                    tok = end;
                }
            } else if tok != t::K_USING {
                if self.tok_is_keyword(tok, t::K_SQLSTATE, "sqlstate") {
                    if self.yylex()? != t::SCONST {
                        return Err(self.yyerror("syntax error"));
                    }
                    let state = self.cur.str();
                    if !valid_sqlstate(&state) {
                        return Err(self.yyerror("invalid SQLSTATE code"));
                    }
                    condname = Some(state);
                } else {
                    let name = if tok == t::T_WORD {
                        word_of(&self.cur)
                    } else if is_unreserved_keyword(tok) {
                        self.cur.str()
                    } else {
                        return Err(self.yyerror("syntax error"));
                    };
                    if !condition_known(&name) {
                        return Err(format!("unrecognized exception condition \"{name}\""));
                    }
                    condname = Some(name);
                }
                tok = self.yylex()?;
                if tok != t::SEMI && tok != t::K_USING {
                    return Err(self.yyerror("syntax error"));
                }
            }
            if tok == t::K_USING {
                options = self.read_raise_options()?;
            }
        }
        check_raise_parameters(&message, params.len())?;
        Ok(Stmt::Raise { lineno, elog_level, condname, message, params, options })
    }

    fn stmt_dynexecute(&mut self, location: i32) -> R<Stmt> {
        let (query, _, mut end) = self.read_sql_construct(t::K_INTO, t::K_USING, t::SEMI, "INTO or USING or ;", PARSE_PLPGSQL_EXPR, true, true)?;
        let lineno = self.lineno(location);
        let (mut into, mut strict, mut target) = (false, false, None);
        let mut params = Vec::new();
        loop {
            if end == t::K_INTO {
                if into {
                    return Err(self.yyerror("syntax error"));
                }
                into = true;
                target = self.read_into_target(Some(&mut strict))?;
                end = self.yylex()?;
            } else if end == t::K_USING {
                if !params.is_empty() {
                    return Err(self.yyerror("syntax error"));
                }
                loop {
                    let (expr, _, e) = self.read_sql_construct(t::COMMA, t::SEMI, t::K_INTO, ", or ; or INTO", PARSE_PLPGSQL_EXPR, true, true)?;
                    params.push(expr);
                    end = e;
                    if end != t::COMMA {
                        break;
                    }
                }
            } else if end == t::SEMI {
                break;
            } else {
                return Err(self.yyerror("syntax error"));
            }
        }
        Ok(Stmt::DynExecute { lineno, query, into, strict, target, params })
    }

    fn stmt_open(&mut self, location: i32, curvar: i32) -> R<Stmt> {
        let lineno = self.lineno(location);
        let mut cursor_options = CURSOR_FAST_PLAN;
        let (mut argquery, mut query, mut dynquery) = (None, None, None);
        let mut params = Vec::new();
        let bound = matches!(&self.datums[curvar as usize], Datum::Var(v) if v.cursor_explicit_expr.is_some());
        if !bound {
            let mut tok = self.yylex()?;
            if self.tok_is_keyword(tok, t::K_NO, "no") {
                tok = self.yylex()?;
                if self.tok_is_keyword(tok, t::K_SCROLL, "scroll") {
                    cursor_options |= CURSOR_NO_SCROLL;
                    tok = self.yylex()?;
                }
            } else if self.tok_is_keyword(tok, t::K_SCROLL, "scroll") {
                cursor_options |= CURSOR_SCROLL;
                tok = self.yylex()?;
            }
            if tok != t::K_FOR {
                return Err(self.yyerror("syntax error, expected \"FOR\""));
            }
            tok = self.yylex()?;
            if tok == t::K_EXECUTE {
                let (expr, mut end) = self.read_sql_expression2(t::K_USING, t::SEMI, "USING or ;")?;
                dynquery = Some(expr);
                if end == t::K_USING {
                    loop {
                        let (expr, e) = self.read_sql_expression2(t::COMMA, t::SEMI, ", or ;")?;
                        params.push(expr);
                        end = e;
                        if end != t::COMMA {
                            break;
                        }
                    }
                }
            } else {
                self.push_back(tok)?;
                query = Some(self.read_sql_stmt()?);
            }
        } else {
            argquery = self.read_cursor_args(curvar, t::SEMI)?;
        }
        Ok(Stmt::Open { lineno, curvar, cursor_options, argquery, query, dynquery, params })
    }

    /// `make_case`: a CASE with a test expression compares each WHEN against a variable of its own.
    fn make_case(&mut self, location: i32, t_expr: Option<Expr>, mut whens: Vec<CaseWhen>, else_stmts: Option<Vec<Stmt>>) -> R<Stmt> {
        let lineno = self.lineno(location);
        let have_else = else_stmts.is_some();
        let mut t_varno = 0;
        if t_expr.is_some() {
            let name = format!("__Case__Variable_{}__", self.datums.len());
            t_varno = self.build_variable(&name, lineno, build_datatype(INT4OID)?, true)?;
            for w in &mut whens {
                w.expr.query = format!("\"{name}\" IN ({})", w.expr.query);
            }
        }
        Ok(Stmt::Case { lineno, t_expr, t_varno, case_when_list: whens, have_else, else_stmts: else_stmts.unwrap_or_default() })
    }
}

fn empty_expr() -> Expr {
    Expr { query: String::new(), parse_mode: 0 }
}

/// A `%TYPE` or `%ROWTYPE` type, which libpg_query names without resolving it.
fn pct_type(typname: String) -> Type {
    Type { typname, typoid: 0, ttype: TType::Scalar, typisarray: false, collatable: false }
}

/// `parse_datatype`: a type's name read by Postgres's parser, then looked up.
fn parse_datatype(text: &str) -> R<Type> {
    if text.bytes().all(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')) {
        return Err(format!("invalid type name \"{text}\""));
    }
    let tr = super::compile::parse_type_name(text)?;
    if tr.setof {
        return Err(format!("invalid type name \"{text}\""));
    }
    let tr = TypeRef { names: tr.names, pct_type: tr.pct_type, array: tr.array, setof: false };
    match lookup_type(&tr)? {
        Some(oid) => build_datatype(oid),
        None => Err(format!("type \"{}\" does not exist", tr.display())),
    }
}

#[allow(dead_code)]
const _: (i32, u32, u8) = (0, RECORDOID, NS_LABEL);
