//! Every catalog query the engine names to its host, read by the parser, against the rules that
//! keep a user's objects from answering it ([`crate::postgres`] states them): each operator,
//! function, type and table is `pg_catalog`'s by name, each function one that takes no polymorphic
//! argument as it is called, no construct looks an operator up by name, and no literal reaches an
//! operator or a function untyped. What the server does with them is the loader's test of hostile
//! objects (`tests/catalog-queries.test.ts`).

use crate::{bodies, capabilities, cardinality, errors, extension, pgtypes, postgres, schema_fingerprint};
use std::collections::BTreeSet;
use syntax::postgres::protobuf::{AExprKind, SubLinkType};
use serde_json::{Map, Value};
use reylo_engine_core::structured;

/// Each query by its name.
fn queries() -> Vec<(&'static str, String)> {
    vec![
        ("ATTNOTNULL_QUERY", postgres::ATTNOTNULL_QUERY.into()),
        ("CHECKS_QUERY", postgres::CHECKS_QUERY.into()),
        ("REFUSES_NULL_QUERY", postgres::REFUSES_NULL_QUERY.into()),
        ("COMMENTS_QUERY", postgres::COMMENTS_QUERY.into()),
        ("RELNAMES_QUERY", postgres::RELNAMES_QUERY.into()),
        ("REQUIRED_COLUMNS_QUERY", postgres::REQUIRED_COLUMNS_QUERY.into()),
        ("RELATION_COLUMNS_QUERY", postgres::RELATION_COLUMNS_QUERY.into()),
        ("TRUST_QUERY", postgres::TRUST_QUERY.into()),
        ("ARBITERS_QUERY", postgres::ARBITERS_QUERY.into()),
        ("TRIGGERS_QUERY", cardinality::TRIGGERS_QUERY.into()),
        ("UNIQUE_KEYS_QUERY", cardinality::UNIQUE_KEYS_QUERY.into()),
        ("COLUMNS_QUERY", cardinality::COLUMNS_QUERY.into()),
        ("FUNCTIONS_QUERY", cardinality::FUNCTIONS_QUERY.into()),
        ("SET_RETURNING_OPERATORS_QUERY", cardinality::SET_RETURNING_OPERATORS_QUERY.into()),
        ("SET_RETURNING_ATTRIBUTES_QUERY", cardinality::SET_RETURNING_ATTRIBUTES_QUERY.into()),
        ("BODIES_QUERY", bodies::BODIES_QUERY.into()),
        ("OPERATOR_FUNCTIONS_QUERY", bodies::OPERATOR_FUNCTIONS_QUERY.into()),
        ("TYPE_QUERY", pgtypes::TYPE_QUERY.into()),
        ("RELATION_TYPES_QUERY", pgtypes::RELATION_TYPES_QUERY.into()),
        ("ERRORS_QUERY", errors::ERRORS_QUERY.into()),
        ("OPERATORS_QUERY", errors::OPERATORS_QUERY.into()),
        ("USER_OPERATORS_QUERY", errors::USER_OPERATORS_QUERY.into()),
        ("PROBE_QUERY", extension::PROBE_QUERY.into()),
        ("PG_TABLE_QUERY", structured::PG_TABLE_QUERY.into()),
        ("TABLES_QUERY", capabilities::TABLES_QUERY.into()),
        ("IMMUTABLE_QUERY", capabilities::IMMUTABLE_QUERY.into()),
        ("EXTENSION_TABLES_QUERY", capabilities::EXTENSION_TABLES_QUERY.into()),
        ("schema_fingerprint::QUERY", schema_fingerprint::QUERY.clone()),
    ]
}

/// The queries the engine sends that are no catalog query: they call the server-side extension's
/// own functions, in its schema, which [`queries`] checks the probe for first.
const EXTENSION_CALLS: [&str; 3] = ["VERSION_QUERY", "ANALYZE_QUERY", "ANSWER_QUERY"];

/// The functions the queries call, each in a form that is an exact match of a built-in taking no
/// polymorphic argument. A query that needs another adds it here once it is checked to be so:
/// `unnest`, `array_agg`, `cardinality`, `array_length`, `to_jsonb` and the like take any type, and
/// an overload of narrower types created in `pg_catalog` would be the closer match.
const FUNCTIONS: [&str; 40] = [
    "bit_or",
    "bool_or",
    "count",
    "current_setting",
    "float4out",
    "format_type",
    "generate_series",
    "has_schema_privilege",
    "int2out",
    "int4out",
    "jsonb_in",
    "max",
    "md5",
    "oidout",
    "pg_get_constraintdef",
    "pg_function_is_visible",
    "pg_get_expr",
    "pg_get_function_sqlbody",
    "pg_get_indexdef",
    "pg_get_ruledef",
    "pg_get_serial_sequence",
    "pg_get_viewdef",
    "pg_operator_is_visible",
    "pg_table_is_visible",
    "quote_literal",
    "regclassout",
    "regcollationout",
    "regnamespaceout",
    "regoperatorout",
    "regprocedureout",
    "regtypeout",
    "regexp_matches",
    "replace",
    "string_agg",
    "textcat",
    "textin",
    "textout",
    "to_number",
    "to_regclass",
    "to_regprocedure",
];

/// A name list's parts, as the parse tree's JSON writes them (`[{"node": {"String": {"sval": …}}}]`).
fn parts(names: Option<&Value>) -> Vec<&str> {
    names.and_then(Value::as_array).into_iter().flatten().filter_map(|n| n.pointer("/node/String/sval").and_then(Value::as_str)).collect()
}

/// Whether a node is a string literal or a NULL with no type of its own.
fn untyped(node: Option<&Value>) -> bool {
    node.and_then(|n| n.pointer("/node/AConst")).is_some_and(|c| c.get("isnull").and_then(Value::as_bool) == Some(true) || c.pointer("/val/Sval").is_some())
}

/// Every object in the tree with the key it is under, depth first.
fn objects<'a>(key: &'a str, value: &'a Value, out: &mut Vec<(&'a str, &'a Map<String, Value>)>) {
    match value {
        Value::Object(map) => {
            out.push((key, map));
            for (k, v) in map {
                objects(k, v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| objects(key, v, out)),
        _ => {}
    }
}

/// What `sql` breaks of the rules, one line each.
fn violations(sql: &str) -> Vec<String> {
    let parsed = syntax::postgres::parse(sql, 0).unwrap_or_else(|e| panic!("{e}\n{sql}"));
    let tree = serde_json::to_value(&parsed.protobuf).unwrap();
    let mut all = Vec::new();
    objects("", &tree, &mut all);
    let ctes: Vec<&str> = all.iter().filter(|(k, _)| *k == "CommonTableExpr").filter_map(|(_, m)| m.get("ctename").and_then(Value::as_str)).collect();
    let catalog = |names: Option<&Value>| parts(names).first() == Some(&"pg_catalog");
    let mut out = Vec::new();
    for (key, m) in &all {
        match *key {
            "AExpr" => {
                let op = parts(m.get("name")).join(".");
                let kind = m.get("kind").and_then(Value::as_i64).unwrap_or(0) as i32;
                if ![AExprKind::AexprOp as i32, AExprKind::AexprOpAny as i32, AExprKind::AexprOpAll as i32].contains(&kind) {
                    out.push(format!("an expression that looks `{op}` up by name ({:?})", AExprKind::try_from(kind)));
                } else if !catalog(m.get("name")) {
                    out.push(format!("the operator `{op}`, not spelled OPERATOR(pg_catalog.{op})"));
                }
                if untyped(m.get("lexpr")) || untyped(m.get("rexpr")) {
                    out.push(format!("an untyped literal beside `{op}`"));
                }
            }
            "SubLink" => {
                let kind = m.get("sub_link_type").and_then(Value::as_i64).unwrap_or(0) as i32;
                if [SubLinkType::AnySublink as i32, SubLinkType::AllSublink as i32].contains(&kind) && !catalog(m.get("oper_name")) {
                    out.push(format!("`{}` ANY or ALL over a subquery (IN), not spelled OPERATOR(pg_catalog.…)", parts(m.get("oper_name")).join(".")));
                }
            }
            "FuncCall" => {
                let name = parts(m.get("funcname"));
                match name.as_slice() {
                    ["pg_catalog", function] if FUNCTIONS.contains(function) => {}
                    ["reylo", _] => {}
                    _ => out.push(format!("the function `{}`, not one of pg_catalog's the queries may call", name.join("."))),
                }
                if m.get("args").and_then(Value::as_array).is_some_and(|args| args.iter().any(|a| untyped(Some(a)))) {
                    out.push(format!("an untyped literal passed to `{}`", name.join(".")));
                }
            }
            "type_name" | "TypeName" if !catalog(m.get("names")) => out.push(format!("the type `{}`, not pg_catalog's", parts(m.get("names")).join("."))),
            "RangeVar" => {
                let schema = m.get("schemaname").and_then(Value::as_str).unwrap_or("");
                let name = m.get("relname").and_then(Value::as_str).unwrap_or("");
                if schema != "pg_catalog" && !(schema.is_empty() && ctes.contains(&name)) {
                    out.push(format!("the table `{name}`, not pg_catalog's"));
                }
            }
            "CaseExpr" if m.get("arg").is_some_and(|a| !a.is_null()) => out.push("a CASE of an operand, which looks `=` up by name".into()),
            "JoinExpr" if m.get("is_natural").and_then(Value::as_bool) == Some(true) || m.get("using_clause").and_then(Value::as_array).is_some_and(|u| !u.is_empty()) => {
                out.push("a NATURAL or USING join, which looks `=` up by name".into())
            }
            _ => {}
        }
    }
    out
}

#[test]
fn every_catalog_query_names_only_pg_catalog_s_objects() {
    let mut failures = Vec::new();
    for (name, sql) in queries() {
        for v in violations(&sql) {
            failures.push(format!("{name}: {v}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_rules_catch_what_a_user_may_capture() {
    let caught = |sql: &str| violations(sql).len();
    assert_eq!(caught("SELECT c.oid FROM pg_catalog.pg_class c WHERE c.oid OPERATOR(pg_catalog.=) '1'::pg_catalog.oid"), 0);
    assert_eq!(caught("SELECT c.oid FROM pg_class c"), 1);
    assert_eq!(caught("SELECT 1 FROM pg_catalog.pg_class c WHERE c.relname = 'x'::pg_catalog.name"), 1);
    assert_eq!(caught("SELECT 1 FROM pg_catalog.pg_class c WHERE c.relkind OPERATOR(pg_catalog.=) 'r'"), 1);
    assert_eq!(caught("SELECT 1 FROM pg_catalog.pg_class c WHERE c.relkind IN ('r'::pg_catalog.\"char\")"), 1);
    assert_eq!(caught("SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid IN (SELECT t.oid FROM pg_catalog.pg_type t)"), 1);
    assert_eq!(caught("SELECT pg_catalog.unnest(c.relacl) FROM pg_catalog.pg_class c"), 1);
    assert_eq!(caught("SELECT pg_catalog.cardinality(ARRAY[1])"), 1);
    assert_eq!(caught("SELECT c.oid::int8 FROM pg_catalog.pg_class c"), 1);
    assert_eq!(caught("SELECT CASE c.relkind WHEN 'r'::pg_catalog.\"char\" THEN 1 END FROM pg_catalog.pg_class c"), 1);
    assert_eq!(caught("SELECT NULLIF(1, 2)"), 1);
    assert_eq!(caught("WITH x AS (SELECT 1 AS n) SELECT x.n FROM x JOIN x y USING (n)"), 1);
}

/// What a query reads of the catalog: the catalogs it names, every column it names (by its own
/// name: a catalog's columns are prefixed by their catalog's, so a name is one catalog's), the
/// settings it asks for, and the functions it calls. The names the query gives its own results
/// and relations (a select list's, a WITH query's and its columns, an alias's) are no column of a
/// catalog's, and are left out.
#[derive(Default, Debug)]
struct Reads {
    catalogs: BTreeSet<String>,
    columns: BTreeSet<String>,
    settings: BTreeSet<String>,
    functions: BTreeSet<String>,
}

/// A string literal, bare or cast.
fn literal(node: &Value) -> Option<&str> {
    node.pointer("/node/AConst/val/Sval/sval").or_else(|| node.pointer("/node/TypeCast/arg/node/AConst/val/Sval/sval")).and_then(Value::as_str)
}

fn reads(sql: &str) -> Reads {
    let parsed = syntax::postgres::parse(sql, 0).unwrap_or_else(|e| panic!("{e}\n{sql}"));
    let tree = serde_json::to_value(&parsed.protobuf).unwrap();
    let mut all = Vec::new();
    objects("", &tree, &mut all);
    let mut own = BTreeSet::new();
    let mut found = Reads::default();
    for (key, m) in &all {
        match *key {
            "ResTarget" => own.extend(m.get("name").and_then(Value::as_str).map(String::from)),
            "CommonTableExpr" => {
                own.extend(m.get("ctename").and_then(Value::as_str).map(String::from));
                own.extend(parts(m.get("aliascolnames")).into_iter().map(String::from));
            }
            "alias" | "Alias" => {
                own.extend(m.get("aliasname").and_then(Value::as_str).map(String::from));
                own.extend(parts(m.get("colnames")).into_iter().map(String::from));
            }
            "RangeVar" if m.get("schemaname").and_then(Value::as_str) == Some("pg_catalog") => {
                found.catalogs.extend(m.get("relname").and_then(Value::as_str).map(String::from));
            }
            "ColumnRef" => found.columns.extend(parts(m.get("fields")).last().map(|c| c.to_string())),
            "FuncCall" => {
                let name = parts(m.get("funcname"));
                if name.as_slice() == ["pg_catalog", "current_setting"] {
                    found.settings.extend(m.get("args").and_then(Value::as_array).and_then(|a| a.first()).and_then(literal).map(String::from));
                }
                found.functions.insert(name.join("."));
            }
            _ => {}
        }
    }
    found.columns.retain(|c| !own.contains(c));
    found
}

/// The columns the engine reads that the fingerprint need not, each with why: its value is in a
/// row the fingerprint has by another column, or it cannot change while its object lasts, and an
/// object made again moves its row by another column.
const UNFINGERPRINTED_COLUMNS: &[(&str, &str)] = &[
    ("atthasdef", "whether a column has a default, which its default's row says"),
    ("conbin", "a constraint's expression, which pg_get_constraintdef writes"),
    ("conkey", "a constraint's columns, which pg_get_constraintdef writes"),
    ("contype", "a constraint's kind, which pg_get_constraintdef writes"),
    ("convalidated", "whether a constraint is validated, which pg_get_constraintdef writes as NOT VALID"),
    ("connoinherit", "whether a constraint is inherited, which pg_get_constraintdef writes as NO INHERIT"),
    ("confrelid", "the table a foreign key refers to, which pg_get_constraintdef writes"),
    ("confdeltype", "a foreign key's ON DELETE, which pg_get_constraintdef writes"),
    ("confupdtype", "a foreign key's ON UPDATE, which pg_get_constraintdef writes"),
    ("indkey", "an index's columns, which pg_get_indexdef writes"),
    ("indnkeyatts", "how many of an index's columns are its key, which pg_get_indexdef writes as INCLUDE"),
    ("indisunique", "whether an index is unique, which pg_get_indexdef writes"),
    ("indisprimary", "whether an index is a primary key's, which the key's constraint row writes"),
    ("indisexclusion", "whether an index is an exclusion constraint's, which the constraint's row writes"),
    ("indimmediate", "whether an index's constraint is checked at once, which the constraint's row writes as DEFERRABLE"),
    ("indpred", "a partial index's predicate, which pg_get_indexdef writes"),
    ("indclass", "an index's operator classes, which pg_get_indexdef writes"),
    ("indcollation", "an index's collations, which pg_get_indexdef writes"),
    ("ev_action", "a rule's query, which pg_get_ruledef and pg_get_viewdef write"),
    ("is_instead", "whether a rule is an INSTEAD rule, which pg_get_ruledef writes"),
    ("tgconstraint", "the constraint of a constraint trigger, which is set when it is made"),
    ("tgparentid", "the trigger a partition's is cloned from, which is set when it is made"),
    ("oprname", "an operator's name, which regoperator writes"),
    ("oprleft", "an operator's left type, which regoperator writes"),
    ("oprright", "an operator's right type, which regoperator writes"),
    ("oprkind", "whether an operator is a prefix one, which regoperator writes as NONE for its left type"),
    ("oprresult", "an operator's result, its function's, which that function's row has"),
    ("proname", "a function's name, which regprocedure writes"),
    ("proargtypes", "a function's arguments, which regprocedure writes"),
    ("pronargs", "how many arguments a function takes, which regprocedure writes"),
    ("prosqlbody", "a function's SQL-standard body, which pg_get_function_sqlbody writes"),
    ("reltype", "a relation's row type, which is set when it is made"),
    ("typarray", "a type's array type, which is set when it is made"),
    ("typelem", "an array type's element, which its name, as regtype writes it, carries"),
    ("typrelid", "a row type's relation, which is set when it is made"),
    ("typinput", "a base type's input function, which is that function's row"),
    ("typdefaultbin", "a type's default, which typdefault writes"),
];

/// The functions the engine calls that the fingerprint need not, each with why.
const UNFINGERPRINTED_FUNCTIONS: &[(&str, &str)] = &[
    ("pg_catalog.bit_or", "an aggregate of the values read"),
    ("pg_catalog.bool_or", "an aggregate of the values read"),
    ("pg_catalog.count", "an aggregate of the values read"),
    ("pg_catalog.generate_series", "a series of numbers"),
    ("pg_catalog.max", "an aggregate of the values read"),
    ("pg_catalog.replace", "a name quoted"),
    ("pg_catalog.textcat", "names joined"),
    ("pg_catalog.oidout", "an oid written as text"),
    ("pg_catalog.quote_literal", "a value written as a literal"),
    ("pg_catalog.regexp_matches", "a pattern matched in text"),
    ("pg_catalog.to_number", "text read as a number"),
    ("pg_catalog.format_type", "a type's name, which every type's row writes"),
    ("pg_catalog.pg_function_is_visible", "whether the search path reaches a function, which the search path and its row's name say"),
    ("pg_catalog.pg_operator_is_visible", "whether the search path reaches an operator, which the search path and its row's name say"),
    ("pg_catalog.to_regclass", "a relation by its name, which the search path, the rows' names and the schemas the role may use resolve"),
    ("pg_catalog.pg_get_serial_sequence", "a column's sequence, which the owned sequences' rows name"),
];

/// Every catalog, column, setting and catalog function an analysis reads, the fingerprint reads
/// too, or one of the lists above says why it need not: a change to what the engine reads that
/// the fingerprint missed would let `reylo/.cache/` serve an analysis of a schema that has changed.
#[test]
fn the_schema_fingerprint_reads_everything_the_engine_reads() {
    let fingerprint = reads(&schema_fingerprint::QUERY);
    let mut missing = Vec::new();
    for (name, sql) in queries() {
        if name == "schema_fingerprint::QUERY" || name == "PROBE_QUERY" {
            continue;
        }
        let engine = reads(&sql);
        for catalog in engine.catalogs.difference(&fingerprint.catalogs) {
            missing.push(format!("{name} reads the catalog {catalog}"));
        }
        for column in engine.columns.difference(&fingerprint.columns) {
            if !UNFINGERPRINTED_COLUMNS.iter().any(|(c, _)| c == column) {
                missing.push(format!("{name} reads the column {column}"));
            }
        }
        for setting in engine.settings.difference(&fingerprint.settings) {
            missing.push(format!("{name} reads the setting {setting}"));
        }
        for function in engine.functions.difference(&fingerprint.functions) {
            if !UNFINGERPRINTED_FUNCTIONS.iter().any(|(f, _)| f == function) {
                missing.push(format!("{name} calls {function}"));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "the schema's fingerprint (schema_fingerprint.rs) reads none of these, and no list here says why it need not:\n{}", missing.join("\n"));
    // A name on a list that the engine no longer reads, or that the fingerprint now does, is left over.
    let read: Vec<Reads> = queries().into_iter().map(|(_, sql)| reads(&sql)).collect();
    for (column, _) in UNFINGERPRINTED_COLUMNS {
        assert!(read.iter().any(|r| r.columns.contains(*column)) && !fingerprint.columns.contains(*column), "{column} is no longer read by the engine alone: take it off UNFINGERPRINTED_COLUMNS");
    }
    for (function, _) in UNFINGERPRINTED_FUNCTIONS {
        assert!(read.iter().any(|r| r.functions.contains(*function)) && !fingerprint.functions.contains(*function), "{function} is no longer called by the engine alone: take it off UNFINGERPRINTED_FUNCTIONS");
    }
}

/// Every query the engine's sources define is one [`queries`] holds, or a call of the server-side
/// extension's, so that a new catalog query is held to the rules and to the fingerprint.
#[test]
fn every_query_the_engine_defines_is_checked() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let checked: Vec<&str> = queries().iter().map(|(name, _)| *name).collect::<Vec<_>>().into_iter().map(|n| n.rsplit("::").next().unwrap()).collect();
    let mut defined = Vec::new();
    for entry in std::fs::read_dir(&root).unwrap().flatten() {
        let source = std::fs::read_to_string(entry.path()).unwrap();
        for line in source.lines() {
            let Some(rest) = line.split("const ").nth(1) else { continue };
            let name: String = rest.chars().take_while(|c| c.is_ascii_uppercase() || *c == '_').collect();
            if name.ends_with("_QUERY") && rest[name.len()..].trim_start().starts_with(':') {
                defined.push(name);
            }
        }
    }
    let unchecked: Vec<&String> = defined.iter().filter(|n| !checked.contains(&n.as_str()) && !EXTENSION_CALLS.contains(&n.as_str())).collect();
    assert!(unchecked.is_empty(), "add each to queries(): {unchecked:?}");
}
