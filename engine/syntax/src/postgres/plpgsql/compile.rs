//! PL/pgSQL's compiler as libpg_query runs it (`pg_query_parse_plpgsql.c`, `pl_comp.c`): each
//! CREATE FUNCTION and DO of a text, its body read by PL/pgSQL's grammar into the tree
//! [`super::nodes`] describes, against the stand-in catalog libpg_query carries.

use super::nodes::{Block, Datum, Function, Row, Stmt, TType, Type};
use super::tables::{CONDITIONS, TYPES};
use crate::postgres::pb;
use crate::postgres::pb::node::Node as N;
use crate::postgres::scan;

pub(super) type R<T> = Result<T, String>;

/// A token code as PL/pgSQL's grammar and actions compare them: a terminal's symbol, a character
/// the grammar has no symbol for as `1000 +` its byte, and 0 at the end of the text.
pub(super) type Code = u16;
pub(super) const EOF: Code = 0;
/// No token read ahead (`YYEMPTY`).
pub(super) const EMPTY: Code = u16::MAX;
pub(super) const fn ch(c: u8) -> Code {
    1000 + c as Code
}
pub(super) const LBRACKET: Code = ch(b'[');
pub(super) const RBRACKET: Code = ch(b']');
pub(super) const DOT: Code = ch(b'.');
pub(super) const PERCENT: Code = ch(b'%');
/// The `Op` terminal, which the generated names leave out for its case.
pub(super) const OP: Code = 10;

/// A few type oids the compiler names.
pub(super) const BOOLOID: u32 = 16;
pub(super) const NAMEOID: u32 = 19;
pub(super) const INT4OID: u32 = 23;
pub(super) const TEXTOID: u32 = 25;
pub(super) const OIDOID: u32 = 26;
pub(super) const RECORDOID: u32 = 2249;
pub(super) const VOIDOID: u32 = 2278;
pub(super) const TRIGGEROID: u32 = 2279;
pub(super) const EVENT_TRIGGEROID: u32 = 3838;
pub(super) const REFCURSOROID: u32 = 1790;
pub(super) const TEXTARRAYOID: u32 = 1009;

/// What a token carries (`YYSTYPE`).
#[derive(Clone, Debug, Default)]
pub(super) enum Val {
    #[default]
    None,
    Str(String),
    Int(i32),
    Keyword(&'static str),
    Word(String),
    CWord(Vec<String>),
    Datum { dno: i32, ident: Option<String>, quoted: bool, idents: Vec<String> },
}

/// A token as PL/pgSQL's scanner hands it over: its code, value, offset and length.
#[derive(Clone, Debug, Default)]
pub(super) struct Tok {
    pub code: Code,
    pub val: Val,
    pub loc: i32,
    pub leng: i32,
}

impl Tok {
    pub fn str(&self) -> String {
        match &self.val {
            Val::Str(s) => s.clone(),
            Val::Keyword(k) => k.to_string(),
            Val::Word(ident) => ident.clone(),
            _ => String::new(),
        }
    }
}

/// How the scanner resolves a name (`IdentifierLookup`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lookup {
    Normal,
    Declare,
    Expr,
}

pub(super) const NS_LABEL: u8 = 0;
pub(super) const NS_VAR: u8 = 1;
pub(super) const NS_REC: u8 = 2;
pub(super) const LABEL_BLOCK: i32 = 0;
pub(super) const LABEL_LOOP: i32 = 1;
pub(super) const LABEL_OTHER: i32 = 2;

/// A namespace entry (`PLpgSQL_nsitem`): a block's label, or a name for a datum.
pub(super) struct NsItem {
    pub kind: u8,
    pub itemno: i32,
    pub name: String,
    pub prev: Option<usize>,
}

/// What the function being compiled returns, as the RETURN statements check it.
#[derive(Default)]
pub(super) struct FnInfo {
    pub retset: bool,
    pub rettype: u32,
    pub procedure: bool,
    pub out_param_varno: i32,
}

/// One function's compilation: the scanner over its body, the namespace and datums its
/// declarations build, and the grammar's lookahead.
pub(super) struct Compiler<'s> {
    pub src: &'s str,
    pub raw: Vec<scan::Token>,
    pub raw_at: usize,
    pub stop: Option<String>,
    pub pushback: Vec<Tok>,
    /// The code `yylex` last returned (`plpgsql_yytoken`).
    pub yytoken: Code,
    /// The length of the token `yylex` last returned (`plpgsql_yyleng`).
    pub yyleng: i32,
    /// `yylval` and `yylloc`: the token `yylex` last returned.
    pub cur: Tok,
    /// The grammar's lookahead (`yychar`).
    pub yychar: Code,
    pub lookup: Lookup,
    pub ns: Vec<NsItem>,
    pub ns_top: Option<usize>,
    pub datums: Vec<Datum>,
    pub datums_last: usize,
    pub func: FnInfo,
    pub result: Option<Block>,
    /// Where each line ends, for line numbers.
    pub newlines: Vec<u32>,
    /// Whether types resolve as in libpg_query's stand-in catalog, which writes a `%ROWTYPE`
    /// variable as a scalar. Otherwise one is a record, as in Postgres, and so is a `%TYPE` of a
    /// record.
    pub stand_in: bool,
}

impl<'s> Compiler<'s> {
    fn new(src: &'s str) -> Self {
        let (raw, stop) = match scan::scan(src) {
            Ok(raw) => (raw, None),
            Err((raw, stop)) => (raw, Some(stop.message)),
        };
        Compiler { src, raw, raw_at: 0, stop, pushback: Vec::new(), yytoken: 0, yyleng: 0, cur: Tok::default(), yychar: EMPTY, lookup: Lookup::Normal, ns: Vec::new(), ns_top: None, datums: Vec::new(), datums_last: 0, func: FnInfo { out_param_varno: -1, ..FnInfo::default() }, result: None, newlines: src.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i as u32).collect(), stand_in: true }
    }

    /// The body's line at a byte offset (`plpgsql_location_to_lineno`).
    pub fn lineno(&self, loc: i32) -> i32 {
        if loc < 0 {
            return 0;
        }
        1 + self.newlines.partition_point(|&n| (n as i64) < loc as i64) as i32
    }

    /// `plpgsql_yyerror`: the words, then the current token.
    pub fn yyerror(&self, message: &str) -> String {
        let at = self.cur.loc.max(0) as usize;
        let b = self.src.as_bytes();
        if at >= b.len() {
            return format!("{message} at end of input");
        }
        let end = (at + self.yyleng.max(0) as usize).min(b.len());
        format!("{message} at or near \"{}\"", String::from_utf8_lossy(&b[at..end]))
    }

    /// Reads the body and adds the RETURN a function falls through to.
    fn parse(&mut self) -> R<Block> {
        self.yyparse()?;
        self.result.take().ok_or_else(|| "plpgsql parser returned 1".to_string())
    }

    fn add_dummy_return(&mut self, mut action: Block) -> Block {
        if action.exceptions.is_some() || action.label.is_some() {
            action = Block { lineno: 0, label: None, body: vec![Stmt::Block(action)], exceptions: None, initvarnos: Vec::new() };
        }
        if !matches!(action.body.last(), Some(Stmt::Return { .. })) {
            action.body.push(Stmt::Return { lineno: 0, expr: None, retvarno: self.func.out_param_varno });
        }
        action
    }

    fn function(self, action: Block, new_varno: i32, old_varno: i32) -> Function {
        Function { new_varno, old_varno, datums: self.datums, action: Some(action) }
    }
}

/// A type name as a statement wrote it, read from either of the trees that hold one.
pub(super) struct TypeRef {
    pub names: Vec<String>,
    pub pct_type: bool,
    pub array: bool,
    pub setof: bool,
}

impl TypeRef {
    pub fn of(t: &pb::TypeName) -> TypeRef {
        TypeRef { names: t.names.iter().map(string_of).collect(), pct_type: t.pct_type, array: !t.array_bounds.is_empty(), setof: t.setof }
    }

    /// `TypeNameToString`.
    pub fn display(&self) -> String {
        let mut s = self.names.join(".");
        if self.pct_type {
            s.push_str("%TYPE");
        }
        if self.array {
            s.push_str("[]");
        }
        s
    }
}

fn string_of(n: &pb::Node) -> String {
    match &n.node {
        Some(N::String(s)) => s.sval.clone(),
        _ => String::new(),
    }
}

/// The stand-in catalog's entry for a type oid.
pub(super) fn type_entry(oid: u32) -> R<&'static (u32, &'static str, u8, u32, bool)> {
    TYPES.iter().find(|e| e.0 == oid).ok_or_else(|| format!("Not implemented (SearchSysCache1 got TYPEOID cache request for type OID {oid})"))
}

/// `build_datatype`: a type of the stand-in catalog as a variable's type.
pub(super) fn build_datatype(oid: u32) -> R<Type> {
    let (oid, name, typtype, _, collatable) = *type_entry(oid)?;
    let ttype = match typtype {
        b'c' => TType::Rec,
        b'p' if oid == RECORDOID => TType::Rec,
        b'p' => TType::Pseudo,
        _ => TType::Scalar,
    };
    Ok(Type { typname: name.to_string(), typoid: oid, ttype, typisarray: false, collatable })
}

/// `LookupTypeName`, over libpg_query's stand-in catalog: a builtin type in `pg_catalog`, and
/// any other name in `public` taken for a row type.
pub(super) fn lookup_type(t: &TypeRef) -> R<Option<u32>> {
    if t.pct_type {
        return Err("Not implemented".to_string());
    }
    let (schema, name) = match t.names.as_slice() {
        [name] => (None, name),
        [schema, name] | [_, schema, name] => (Some(schema.as_str()), name),
        _ => return Err(format!("improper qualified name (too many dotted names): {}", t.names.join("."))),
    };
    let builtin = || TYPES.iter().find(|e| e.1 == name).map_or(0, |e| e.0);
    let mut oid = match schema {
        None => match builtin() {
            0 => RECORDOID,
            oid => oid,
        },
        Some("pg_catalog") => builtin(),
        Some("public") => RECORDOID,
        Some(_) => return Err("Not implemented (LookupExplicitNamespace only supports pg_catalog and public)".to_string()),
    };
    if t.array {
        oid = type_entry(oid)?.3;
    }
    Ok((oid != 0).then_some(oid))
}

/// `format_type_be`, for the types of the stand-in catalog.
pub(super) fn format_type(oid: u32) -> R<String> {
    let name = type_entry(oid)?.1;
    Ok(match name {
        "bit" => "bit".into(),
        "bool" => "boolean".into(),
        "bpchar" => "character".into(),
        "float4" => "real".into(),
        "float8" => "double precision".into(),
        "int2" => "smallint".into(),
        "int4" => "integer".into(),
        "int8" => "bigint".into(),
        "numeric" => "numeric".into(),
        "interval" => "interval".into(),
        "time" => "time without time zone".into(),
        "timetz" => "time with time zone".into(),
        "timestamp" => "timestamp without time zone".into(),
        "timestamptz" => "timestamp with time zone".into(),
        "varbit" => "bit varying".into(),
        "varchar" => "character varying".into(),
        name => quote_identifier(name),
    })
}

/// `quote_identifier`: the name, in double quotes where Postgres's parser would not read it bare.
pub(super) fn quote_identifier(name: &str) -> String {
    let b = name.as_bytes();
    let plain = !b.is_empty() && (b[0].is_ascii_lowercase() || b[0] == b'_') && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_');
    let safe = plain && scan::keyword(b).is_none_or(|k| k.2 == 0);
    if safe {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

pub(super) fn condition_known(name: &str) -> bool {
    CONDITIONS.iter().any(|c| c.0 == name)
}

/// `IsPolymorphicType`, and the type a validator puts in its place.
fn polymorphic_stand_in(oid: u32) -> Option<u32> {
    match oid {
        2283 | 2776 | 3500 | 5077 | 5079 => Some(INT4OID),
        2277 | 5078 => Some(1007),
        3831 | 5080 => Some(3904),
        4537 | 4538 => Some(4451),
        _ => None,
    }
}

/// `FunctionParameterMode`, as `pg_proc.proargmodes` spells it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    In,
    Out,
    InOut,
    Variadic,
    Table,
}

struct Param {
    name: String,
    oid: u32,
    mode: Mode,
}

/// `interpret_function_parameter_list`, with libpg_query's stand-ins: each parameter's name, type
/// and mode, and the result type the OUT parameters require.
fn parameters(stmt: &pb::CreateFunctionStmt) -> R<(Vec<Param>, bool, u32)> {
    use pb::FunctionParameterMode as M;
    let mut out = Vec::new();
    let mut in_count = 0;
    let mut out_count = 0;
    let mut var_count = 0;
    let mut have_names = false;
    let mut have_defaults = false;
    let mut required = 0u32;
    let procedure = stmt.is_procedure;
    let fps: Vec<&pb::FunctionParameter> = stmt.parameters.iter().filter_map(|n| if let Some(N::FunctionParameter(p)) = &n.node { Some(p.as_ref()) } else { None }).collect();
    let mode_of = |m: i32| match M::try_from(m).unwrap_or(M::FuncParamIn) {
        M::FuncParamOut => Mode::Out,
        M::FuncParamInout => Mode::InOut,
        M::FuncParamVariadic => Mode::Variadic,
        M::FuncParamTable => Mode::Table,
        _ => Mode::In,
    };
    for (i, fp) in fps.iter().enumerate() {
        let mode = mode_of(fp.mode);
        let tr = fp.arg_type.as_ref().map(TypeRef::of).unwrap_or(TypeRef { names: Vec::new(), pct_type: false, array: false, setof: false });
        let oid = match lookup_type(&tr)? {
            Some(oid) => oid,
            None => return Err(format!("type {} does not exist", tr.display())),
        };
        if tr.setof {
            return Err(if procedure { "procedures cannot accept set arguments" } else { "functions cannot accept set arguments" }.to_string());
        }
        let mut isinput = false;
        if mode != Mode::Out && mode != Mode::Table {
            if var_count > 0 {
                return Err("VARIADIC parameter must be the last input parameter".to_string());
            }
            in_count += 1;
            isinput = true;
        }
        if mode != Mode::In && mode != Mode::Variadic {
            if procedure {
                if var_count > 0 {
                    return Err("VARIADIC parameter must be the last parameter".to_string());
                }
                required = RECORDOID;
            } else if out_count == 0 {
                required = oid;
            }
            out_count += 1;
        }
        if mode == Mode::Variadic {
            var_count += 1;
            // The stand-in catalog records no element types, so only the polymorphic arrays pass.
            if !matches!(oid, 2277 | 5078 | 2276) {
                return Err("VARIADIC parameter must be an array".to_string());
            }
        }
        if !fp.name.is_empty() {
            for prev in &fps[..i] {
                let pm = mode_of(prev.mode);
                let input = |m: Mode| m == Mode::In || m == Mode::Variadic;
                let output = |m: Mode| m == Mode::Out || m == Mode::Table;
                if (input(mode) && output(pm)) || (input(pm) && output(mode)) {
                    continue;
                }
                if prev.name == fp.name {
                    return Err(format!("parameter name \"{}\" used more than once", fp.name));
                }
            }
            have_names = true;
        }
        if fp.defexpr.is_some() {
            if !isinput {
                return Err("only input parameters can have default values".to_string());
            }
            have_defaults = true;
        } else {
            if isinput && have_defaults {
                return Err("input parameters after one with a default value must also have defaults".to_string());
            }
            if procedure && have_defaults {
                return Err("procedure OUT parameters cannot appear after one with a default value".to_string());
            }
        }
        out.push(Param { name: fp.name.clone(), oid, mode });
    }
    if out_count > 1 {
        required = RECORDOID;
    }
    let _ = in_count;
    Ok((out, have_names, required))
}

fn options(options: &[pb::Node]) -> (Option<String>, Option<String>) {
    let mut source = None;
    let mut language = None;
    for o in options {
        let Some(N::DefElem(d)) = &o.node else { continue };
        let arg = d.arg.as_deref().and_then(|a| a.node.as_ref());
        match (d.defname.as_str(), arg) {
            ("as", Some(N::List(l))) => {
                if let Some(last) = l.items.last() {
                    source = Some(string_of(last));
                }
            }
            ("as", Some(N::String(s))) => source = Some(s.sval.clone()),
            ("language", Some(N::String(s))) => language = Some(s.sval.clone()),
            _ => {}
        }
    }
    (source, language)
}

/// The function libpg_query writes for a body in another language: nothing but an empty list of
/// datums.
fn empty() -> Function {
    Function { new_varno: 0, old_varno: 0, datums: Vec::new(), action: None }
}

/// `compile_create_function_stmt_via_callback`.
fn create_function(stmt: &pb::CreateFunctionStmt) -> R<Function> {
    let (source, language) = options(&stmt.options);
    if language.as_deref().unwrap_or("plpgsql") != "plpgsql" {
        return Ok(empty());
    }
    if stmt.sql_body.is_none() && source.is_none() {
        return Err("no function body specified".to_string());
    }
    if stmt.sql_body.is_some() && source.is_some() {
        return Err("duplicate function body specified".to_string());
    }
    if stmt.sql_body.is_some() {
        return Err("inline SQL function body only valid for language SQL".to_string());
    }
    let source = source.unwrap_or_default();
    let names: Vec<String> = stmt.return_type.as_ref().map_or(Vec::new(), |t| t.names.iter().map(string_of).collect());
    let dml_trigger = names.iter().any(|n| n == "trigger");
    let event_trigger = !dml_trigger && names.iter().any(|n| n == "event_trigger");
    let (params, have_names, required) = parameters(stmt)?;
    let (rettype, retset) = if stmt.is_procedure {
        (if required != 0 { required } else { VOIDOID }, false)
    } else if let Some(rt) = &stmt.return_type {
        let tr = TypeRef::of(rt);
        let Some(oid) = lookup_type(&tr)? else {
            return Err(format!("type \"{}\" does not exist", tr.display()));
        };
        if required != 0 && oid != required {
            return Err(format!("function result type must be {} because of OUT parameters", format_type(required)?));
        }
        (oid, tr.setof)
    } else if required != 0 {
        (required, false)
    } else {
        return Err("function result type must be specified".to_string());
    };
    let funcname = stmt.funcname.first().map(string_of).unwrap_or_default();
    let pronargs = params.iter().filter(|p| p.mode != Mode::Out && p.mode != Mode::Table).count();

    if dml_trigger {
        if pronargs != 0 {
            return Err("trigger functions cannot have declared arguments".to_string());
        }
        return trigger_named(&source, &funcname, true);
    }
    let mut c = Compiler::new(&source);
    c.func.procedure = stmt.is_procedure;
    c.ns_push(Some(&funcname), LABEL_BLOCK);
    let mut num_out = 0;
    if event_trigger {
        c.func.rettype = VOIDOID;
        c.func.retset = false;
        if pronargs != 0 {
            return Err("event trigger functions cannot have declared arguments".to_string());
        }
        for (name, promise) in [("tg_event", 10), ("tg_tag", 11)] {
            let dno = c.build_variable(name, 0, build_datatype(TEXTOID)?, true)?;
            c.set_promise(dno, promise);
        }
    } else {
        let mut out_vars = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let buf = format!("${}", i + 1);
            let oid = polymorphic_stand_in(p.oid).unwrap_or(p.oid);
            let dtype = build_datatype(oid)?;
            if dtype.ttype == TType::Pseudo {
                return Err(format!("PL/pgSQL functions cannot accept type {}", format_type(oid)?));
            }
            let named = have_names && !p.name.is_empty();
            let dno = c.build_variable(if named { &p.name } else { &buf }, 0, dtype, false)?;
            let kind = if matches!(c.datums[dno as usize], Datum::Rec(_)) { NS_REC } else { NS_VAR };
            if matches!(p.mode, Mode::Out | Mode::InOut | Mode::Table) {
                out_vars.push(dno);
            }
            c.add_parameter_name(kind, dno, &buf)?;
            if named {
                c.add_parameter_name(kind, dno, &p.name)?;
            }
        }
        num_out = out_vars.len();
        if num_out > 1 || (num_out == 1 && stmt.is_procedure) {
            let fields = out_vars.iter().map(|&d| (c.refname(d), d)).collect();
            let row = c.adddatum(Datum::Row(Row { refname: "(unnamed row)".into(), lineno: -1, fields }));
            c.func.out_param_varno = row;
        } else if num_out == 1 {
            c.func.out_param_varno = out_vars[0];
        }
        let resolved = polymorphic_stand_in(rettype).unwrap_or(rettype);
        c.func.rettype = resolved;
        c.func.retset = retset;
        let (_, _, typtype, _, _) = *type_entry(resolved)?;
        if typtype == b'p' && resolved != VOIDOID && resolved != RECORDOID {
            return Err(if resolved == TRIGGEROID || resolved == EVENT_TRIGGEROID { "trigger functions can only be called as triggers".to_string() } else { format!("PL/pgSQL functions cannot return type {}", format_type(resolved)?) });
        }
        if polymorphic_stand_in(rettype).is_some() && num_out == 0 {
            c.build_variable("$0", 0, build_datatype(resolved)?, true)?;
        }
    }
    c.build_variable("found", 0, build_datatype(BOOLOID)?, true)?;
    let mut action = c.parse()?;
    if num_out > 0 || c.func.rettype == VOIDOID || c.func.retset {
        action = c.add_dummy_return(action);
    }
    Ok(c.function(action, 0, 0))
}

/// A DML trigger function's body compiled, with NEW, OLD and the trigger's other variables.
/// A DML trigger function's body compiled as Postgres compiles it, a `%ROWTYPE` variable a record.
pub(super) fn trigger(source: &str) -> R<Function> {
    trigger_named(source, "reylo_trigger", false)
}

fn trigger_named(source: &str, funcname: &str, stand_in: bool) -> R<Function> {
    let mut c = Compiler::new(source);
    c.stand_in = stand_in;
    c.ns_push(Some(funcname), LABEL_BLOCK);
    let new_varno = c.build_record("new", 0, true);
    let old_varno = c.build_record("old", 0, true);
    for (name, oid, promise) in [("tg_name", NAMEOID, 1), ("tg_when", TEXTOID, 2), ("tg_level", TEXTOID, 3), ("tg_op", TEXTOID, 4), ("tg_relid", OIDOID, 5), ("tg_relname", NAMEOID, 6), ("tg_table_name", NAMEOID, 6), ("tg_table_schema", NAMEOID, 7), ("tg_nargs", INT4OID, 8), ("tg_argv", TEXTARRAYOID, 9)] {
        let dno = c.build_variable(name, 0, build_datatype(oid)?, true)?;
        c.set_promise(dno, promise);
    }
    c.build_variable("found", 0, build_datatype(BOOLOID)?, true)?;
    let action = c.parse()?;
    Ok(c.function(action, new_varno, old_varno))
}

/// `compile_do_stmt` and `plpgsql_compile_inline`.
fn do_block(stmt: &pb::DoStmt) -> R<Function> {
    let (mut source, mut language) = (None, None);
    for a in &stmt.args {
        let Some(N::DefElem(d)) = &a.node else { continue };
        if let Some(N::String(s)) = d.arg.as_deref().and_then(|a| a.node.as_ref()) {
            match d.defname.as_str() {
                "as" => source = Some(s.sval.clone()),
                "language" => language = Some(s.sval.clone()),
                _ => {}
            }
        }
    }
    let Some(source) = source else {
        return Err("no inline code specified".to_string());
    };
    if language.as_deref().unwrap_or("plpgsql") != "plpgsql" {
        return Ok(empty());
    }
    let mut c = Compiler::new(&source);
    c.ns_push(Some("inline_code_block"), LABEL_BLOCK);
    c.func.rettype = VOIDOID;
    c.build_variable("found", 0, build_datatype(BOOLOID)?, true)?;
    let action = c.parse()?;
    let action = c.add_dummy_return(action);
    Ok(c.function(action, 0, 0))
}

/// Each CREATE FUNCTION and DO statement of `sql` compiled, as libpg_query's
/// `pg_query_parse_plpgsql` writes them: a JSON array of functions, or the first error's message.
/// A RETURN and a RETURN NEXT also carry the datum they return, `retvarno`, as they do with
/// pganalyze/libpg_query#402.
pub fn parse_plpgsql(sql: &str) -> Result<String, String> {
    crate::postgres::on_deep_stack(|| {
        let tree = crate::postgres::parse(sql, 0).map_err(|e| match e {
            crate::postgres::Error::Parse(m) | crate::postgres::Error::Scan(m) => m,
        })?;
        let mut out = String::from("[\n");
        let mut any = false;
        for raw in &tree.protobuf.stmts {
            let function = match raw.stmt.as_deref().and_then(|s| s.node.as_ref()) {
                Some(N::CreateFunctionStmt(f)) => create_function(f)?,
                Some(N::DoStmt(d)) => do_block(d)?,
                _ => continue,
            };
            any = true;
            out.push_str(&function.to_json());
            out.push_str(",\n");
        }
        if !any {
            return Ok("[]".to_string());
        }
        out.truncate(out.len() - 2);
        out.push_str("\n]");
        Ok(out)
    })
}

/// `typeStringToTypeName`'s parse: the names of the type Postgres's parser reads in `text`.
pub(super) fn parse_type_name(text: &str) -> R<TypeRef> {
    use crate::postgres::generated_names::{f, t as tags};
    use crate::postgres::nodes::V;
    let list = crate::postgres::parse_in_mode(text, super::nodes::PARSE_TYPE_NAME)?;
    let V::List(items) = list else { return Err(format!("invalid type name \"{text}\"")) };
    let Some(node) = items.borrow().first().cloned() else { return Err(format!("invalid type name \"{text}\"")) };
    if !node.is_a(tags::T_TypeName) {
        return Err(format!("invalid type name \"{text}\""));
    }
    let names = match node.get(f::F_names) {
        V::List(names) => names.borrow().iter().map(|n| match n.get(f::F_sval) {
            V::Str(s) => String::from_utf8_lossy(&s).into_owned(),
            _ => String::new(),
        }).collect(),
        _ => Vec::new(),
    };
    let array = matches!(node.get(f::F_arrayBounds), V::List(l) if !l.borrow().is_empty());
    Ok(TypeRef { names, pct_type: node.get(f::F_pct_type).truthy(), array, setof: node.get(f::F_setof).truthy() })
}

#[cfg(test)]
mod tests {
    use super::parse_plpgsql;

    #[test]
    fn op_is_the_operator_terminal() {
        assert_eq!(super::super::tables::NAMES[super::OP as usize], "Op");
    }

    /// libpg_query's output for the same statements, field for field.
    #[test]
    fn writes_what_libpg_query_writes() {
        assert_eq!(
            parse_plpgsql("DO $$ BEGIN RAISE NOTICE 'hi %', 1; END $$").unwrap(),
            r#"[
{"PLpgSQL_function":{"datums":[{"PLpgSQL_var":{"refname":"found","datatype":{"PLpgSQL_type":{"typname":"bool"}}}}],"action":{"PLpgSQL_stmt_block":{"lineno":1,"body":[{"PLpgSQL_stmt_raise":{"lineno":1,"elog_level":18,"message":"hi %","params":[{"PLpgSQL_expr":{"query":"1","parseMode":2}}]}},{"PLpgSQL_stmt_return":{"retvarno":-1}}]}}}}
]"#
        );
        assert_eq!(
            parse_plpgsql("CREATE FUNCTION f() RETURNS SETOF int LANGUAGE plpgsql AS $$ BEGIN RETURN QUERY SELECT 1; CASE 2 WHEN 1, 2 THEN NULL; ELSE RETURN NEXT 3; END CASE; END $$").unwrap(),
            r#"[
{"PLpgSQL_function":{"datums":[{"PLpgSQL_var":{"refname":"found","datatype":{"PLpgSQL_type":{"typname":"bool"}}}},{"PLpgSQL_var":{"refname":"__Case__Variable_1__","lineno":1,"datatype":{"PLpgSQL_type":{"typname":"int4"}}}}],"action":{"PLpgSQL_stmt_block":{"lineno":1,"body":[{"PLpgSQL_stmt_return_query":{"lineno":1,"query":{"PLpgSQL_expr":{"query":"SELECT 1","parseMode":0}}}},{"PLpgSQL_stmt_case":{"lineno":1,"t_expr":{"PLpgSQL_expr":{"query":"2","parseMode":2}},"t_varno":1,"case_when_list":[{"PLpgSQL_case_when":{"lineno":1,"expr":{"PLpgSQL_expr":{"query":"\"__Case__Variable_1__\" IN (1, 2)","parseMode":2}}}}],"have_else":true,"else_stmts":[{"PLpgSQL_stmt_return_next":{"lineno":1,"expr":{"PLpgSQL_expr":{"query":"3","parseMode":2}}}}]}},{"PLpgSQL_stmt_return":{"retvarno":-1}}]}}}}
]"#
        );
        assert_eq!(parse_plpgsql("SELECT 1").unwrap(), "[]");
        assert_eq!(parse_plpgsql("CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'select 1'").unwrap(), "[\n{\"PLpgSQL_function\":{\"datums\":[]}}\n]");
    }

    /// A trigger's NEW and OLD come first, and a field of NEW is a datum of its own.
    #[test]
    fn reads_a_trigger() {
        let json = parse_plpgsql("CREATE FUNCTION t() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.a := 1; SELECT 1 INTO STRICT NEW.b; RETURN NEW; END $$").unwrap();
        assert!(json.contains(r#"{"PLpgSQL_function":{"old_varno":1,"datums":[{"PLpgSQL_rec":{"refname":"new"}},{"PLpgSQL_rec":{"refname":"old","dno":1}},"#), "{json}");
        assert!(json.contains(r#"{"PLpgSQL_stmt_assign":{"lineno":1,"varno":13,"expr":{"PLpgSQL_expr":{"query":"NEW.a := 1","parseMode":4}}}}"#), "{json}");
        assert!(json.contains(r#""into":true,"strict":true,"target":{"PLpgSQL_row":{"refname":"(unnamed row)","lineno":1,"fields":[{"name":"new.b","varno":14}]}}"#), "{json}");
    }

    /// What PL/pgSQL's compiler refuses, in its words, the SQL inside a body included.
    #[test]
    fn refuses_as_plpgsql_does() {
        for (sql, message) in [
            ("CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$ BEGIN EXIT; END $$", "EXIT cannot be used outside a loop, unless it has a label"),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1 +; END $$", "syntax error at end of input"),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RAISE NOTICE WARNING 'x'; END $$", "unrecognized exception condition \"warning\""),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$ DECLARE x int; x int; BEGIN END $$", "duplicate declaration at or near \"x\""),
            ("DO $$ BEGIN NULL END $$", "syntax error at or near \"END\""),
        ] {
            assert_eq!(parse_plpgsql(sql).unwrap_err(), message, "{sql}");
        }
    }
}
