//! `reylo analyze --format typescript`: each statement's TypeScript types, its parameters and what
//! `.run()` resolves to, as the type check declares them for an imported query. A port of the
//! content mapper's renderer, which the binary carries so that an install with no Node prints them
//! too: `src/render.ts` (the type of each descriptor), `columnKind` and `kindType` in
//! `src/values.ts` (the one value `.run()` returns for each), the project file's `js.codecs` from
//! `src/codecs.ts`, `src/module-types.ts` (the Params and Row types) and `src/type-format.ts` (the
//! layout, and the cardinality applied). It reads the analysis as JSON, as the content mapper reads
//! it, and `tests/analyze-typescript.test.ts` holds its output to theirs.

use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::LazyLock;

/// The column the types are laid out within.
pub const WIDTH: usize = 80;

/// The statement's `Params` and the type `.run()` resolves to, each laid out within [`WIDTH`]
/// columns, as `reylo play` shows them (src/play-result.ts); `None` for parameters where it takes none.
pub struct Printed {
    pub params: Option<String>,
    pub returns: String,
}

impl Printed {
    /// What a statement the engine could not analyze declares.
    pub fn unknown() -> Printed {
        Printed { params: Some("unknown".into()), returns: "unknown".into() }
    }

    /// The declarations, a line each or more, each ending in a newline.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if let Some(params) = &self.params {
            out.push_str(&format!("type Params = {params};\n"));
        }
        out.push_str(&format!("type Returns = {};\n", self.returns));
        out
    }
}

/// A statement's types from its analysis (core's `Analysis` as JSON), on `dialect`, with the
/// project file's `js.codecs` (as JSON) and its `allow_nested_arrays`.
pub fn printed(analysis: &Value, dialect: &str, codecs: &Value, allow_nested_arrays: bool) -> Printed {
    let types = module_types(analysis, dialect, codecs, allow_nested_arrays && dialect == "postgres");
    Printed {
        params: (types.params != "Record<never, never>").then(|| format_type(&types.params, WIDTH)),
        returns: format_type(&if types.analyzed { result_type(&types.row, &types.cardinality) } else { "unknown".to_string() }, WIDTH),
    }
}

// ---------------------------------------------------------------------------------------------
// The descriptor's parts (src/engine-type.ts).

fn kind(t: &Value) -> &str {
    t["kind"].as_str().unwrap_or_default()
}

fn label(t: &Value) -> Option<&str> {
    t.get("label").and_then(Value::as_str)
}

fn is_true(v: &Value) -> bool {
    v.as_bool() == Some(true)
}

/// A JavaScript value's truth: present, and neither false, null, 0 nor "".
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn items(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

static BARE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:^|\.)("(?:[^"]|"")*"|[^."]*)$"#).unwrap());

/// A type's name without the schema that qualifies it: `vector` of `public.vector`, `"Odd"` of `app."Odd"` unquoted.
fn bare_name(t: &Value) -> String {
    let name = label(t).unwrap_or_default();
    let last = BARE.captures(name).and_then(|c| c.get(1)).map_or(name, |m| m.as_str());
    if last.starts_with('"') && last.len() >= 2 {
        last[1..last.len() - 1].replace("\"\"", "\"")
    } else {
        last.to_string()
    }
}

/// Whether a type is a MySQL `SET`.
fn is_mysql_set(t: &Value) -> bool {
    kind(t) == "array" && is_true(&t["distinct"]) && label(t) == Some("set")
}

/// A type with each domain it names written as the base type the domain is made from.
fn with_bases(t: &Value, domains: &Map<String, Value>) -> Value {
    let mut t = t.clone();
    if let Some(base) = label(&t).and_then(|l| domains.get(l)).cloned() {
        t["label"] = base;
    }
    let slot = |s: &mut Value| {
        if s.get("type").is_some() {
            s["type"] = with_bases(&s["type"], domains);
        }
    };
    let each = |list: &mut Value, f: &dyn Fn(&mut Value)| {
        if let Some(items) = list.as_array_mut() {
            items.iter_mut().for_each(f);
        }
    };
    match kind(&t).to_string().as_str() {
        "enum" => t["type"] = with_bases(&t["type"], domains),
        "array" => slot(&mut t["element"]),
        "struct" => {
            each(&mut t["fields"], &slot);
            if t.get("extra").is_some() {
                slot(&mut t["extra"]);
            }
        }
        "table" => each(&mut t["columns"], &slot),
        "tuple" => {
            each(&mut t["elements"], &slot);
            if t.get("optional_elements").is_some() {
                each(&mut t["optional_elements"], &slot);
            }
            if t.get("rest").is_some() {
                slot(&mut t["rest"]);
            }
        }
        "json" => {
            if t.get("content").is_some() {
                slot(&mut t["content"]);
            }
        }
        "union" | "intersection" => each(&mut t["members"], &|m| *m = with_bases(m, domains)),
        "tagged_union" => each(&mut t["alternatives"], &slot),
        "map" => {
            slot(&mut t["key"]);
            slot(&mut t["value"]);
        }
        "range" | "multirange" => t["subtype"] = with_bases(&t["subtype"], domains),
        _ => {}
    }
    t
}

/// The key that marks an array the project lets nest at any depth, which no descriptor holds.
const NESTS: &str = "\u{0}nests";

fn nests_any_deep(t: &Value) -> bool {
    t.get(NESTS).is_some()
}

/// A Postgres array type, by its name, at its innermost level: its element is no Postgres array
/// type, though it may be one the format structures as an array (`bit`, `vector`, `lseg`), or it is
/// an array of whole arrays.
fn postgres_array(t: &Value) -> bool {
    let named = |t: &Value| kind(t) == "array" && bare_name(t).starts_with('_');
    named(t) && (!named(&t["element"]["type"]) || holds_whole_arrays(t))
}

/// Whether a Postgres array's elements are each a whole array of their own, as an array of a
/// domain over an array type is (src/engine-type.ts, `holdsWholeArrays`).
fn holds_whole_arrays(t: &Value) -> bool {
    let named = |t: &Value| kind(t) == "array" && bare_name(t).starts_with('_');
    named(t) && named(&t["element"]["type"]) && label(&t["element"]["type"]) != label(t)
}

/// The type with each Postgres array marked to nest; a parameter's array of whole arrays, or of
/// JSON, keeps one level outside them, which it writes each element of whole (src/engine-type.ts, `markNesting`).
fn mark_nesting(t: &Value, parameter: bool) -> Value {
    let mut out = t.clone();
    match kind(t) {
        "array" => {
            out["element"]["type"] = mark_nesting(&t["element"]["type"], parameter);
            if postgres_array(t) && !(parameter && (holds_whole_arrays(t) || kind(&t["element"]["type"]) == "json")) {
                out[NESTS] = Value::Bool(true);
            }
        }
        "struct" => {
            if let Some(fields) = out["fields"].as_array_mut() {
                for f in fields {
                    f["type"] = mark_nesting(&f["type"], parameter);
                }
            }
        }
        "json" if t.get("content").is_some() => out["content"]["type"] = mark_nesting(&t["content"]["type"], parameter),
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The value `.run()` returns for each type (src/values.ts).

const POINT: &str = "{ x: number; y: number }";

/// The TypeScript type of each scalar kind.
fn ts_type(kind: &str) -> Option<String> {
    Some(match kind {
        "bool" => "boolean".into(),
        "int" | "float" => "number".into(),
        "int8" => "bigint".into(),
        "decimal" | "money" | "text" | "date" | "timestamp" | "timestamptz" | "interval" | "enum" => "string".into(),
        "json" => JSON_TYPE.into(),
        "bytes" => "Uint8Array".into(),
        "vector" => "number[]".into(),
        "hstore" => "Record<string, string | null>".into(),
        "point" => POINT.into(),
        "line" => "{ a: number; b: number; c: number }".into(),
        "lseg" | "polygon" => format!("{POINT}[]"),
        "box" => format!("{{ high: {POINT}; low: {POINT} }}"),
        "path" => format!("{{ closed: boolean; points: {POINT}[] }}"),
        "circle" => format!("{{ center: {POINT}; radius: number }}"),
        "set" => "Set<string>".into(),
        "geometry" | "unknown" => "unknown".into(),
        _ => return None,
    })
}

/// Whether a SQLite declared type names a 64-bit integer.
fn sqlite_wide(declared: &str) -> bool {
    let base = declared.split('(').next().unwrap_or_default().trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    base == "bigint" || base == "int8" || base == "unsigned big int"
}

/// SQLite's affinity of a declared type, by its five rules.
fn sqlite_affinity(declared: &str) -> &'static str {
    let d = declared.to_uppercase();
    if d.contains("INT") {
        "integer"
    } else if d.contains("CHAR") || d.contains("CLOB") || d.contains("TEXT") {
        "text"
    } else if d.contains("BLOB") || d.is_empty() {
        "blob"
    } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
        "real"
    } else {
        "numeric"
    }
}

static BOOLEAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^bool(ean)?$").unwrap());

fn sqlite_declared_kind(declared: &str) -> String {
    let d = declared.trim();
    if BOOLEAN.is_match(d) {
        return "bool".into();
    }
    match sqlite_affinity(d) {
        "integer" => if sqlite_wide(d) { "int8" } else { "int" }.into(),
        "real" => "float".into(),
        "text" => "text".into(),
        "blob" => "bytes".into(),
        _ => "int|float|text".into(),
    }
}

const GEOMETRIC: [&str; 7] = ["point", "line", "lseg", "box", "path", "polygon", "circle"];

fn geometric(t: &Value) -> Option<&str> {
    label(t).filter(|l| GEOMETRIC.contains(l))
}

fn is_row_kind(kind: &str) -> bool {
    kind.starts_with('(')
}

fn delimiter_of(element: &Value) -> Option<&'static str> {
    if geometric(element) == Some("box") {
        return Some(";");
    }
    (kind(element) == "geometry" && matches!(bare_name(element).as_str(), "geometry" | "geography")).then_some(":")
}

fn either(kinds: Vec<String>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in kinds.iter().flat_map(|k| k.split('|')) {
        if !parts.iter().any(|p| p == part) {
            parts.push(part.to_string());
        }
    }
    if parts.iter().any(|p| p == "int8") && !parts.iter().any(|p| p == "float") {
        parts.retain(|p| p != "int");
    }
    parts.join("|")
}

/// The kind of a value of type `t` standing in a result row (src/values.ts, `columnKind`).
fn column_kind(t: &Value, dialect: &str) -> String {
    let name = bare_name(t);
    match kind(t) {
        "boolean" => "bool".into(),
        "integer" => {
            if dialect == "sqlite" {
                return if sqlite_wide(&name) { "int8" } else { "int" }.into();
            }
            if dialect == "mysql" && name == "tinyint(1)" {
                return "bool".into();
            }
            if t["bits"].as_f64().unwrap_or(0.0) > 32.0 { "int8" } else { "int" }.into()
        }
        "float" => "float".into(),
        "decimal" => "decimal".into(),
        "bytes" => if dialect == "postgres" && (name == "macaddr" || name == "macaddr8") { "text" } else { "bytes" }.into(),
        "array" => {
            if is_mysql_set(t) {
                return if dialect == "mysql" { "set" } else { "text" }.into();
            }
            if dialect == "postgres" {
                if name == "vector" || name == "halfvec" {
                    return "vector".into();
                }
                if name == "bit" || name == "varbit" || name == "sparsevec" {
                    return "text".into();
                }
                if let Some(shape) = geometric(t) {
                    return shape.into();
                }
                let element = &t["element"]["type"];
                if kind(element) == "opaque" && bare_name(element) == "record" {
                    return "text".into();
                }
                if holds_whole_arrays(t) {
                    let inner = column_kind(element, dialect);
                    return if inner.ends_with("[]") && delimiter_of(&element["element"]["type"]).is_none() { format!("{{{inner}}}[]") } else { "text".into() };
                }
            }
            if dialect == "mysql" && name == "bit" {
                return if t["size"]["exact"].as_f64() == Some(1.0) { "bool" } else { "bytes" }.into();
            }
            format!("{}[]", column_kind(&t["element"]["type"], dialect))
        }
        "json" => "json".into(),
        "date" => "date".into(),
        "datetime" => "timestamp".into(),
        "instant" => if dialect == "mysql" { "timestamp" } else { "timestamptz" }.into(),
        "duration" => if dialect == "mysql" { "text" } else { "interval" }.into(),
        "map" => if dialect == "postgres" && name == "hstore" { "hstore" } else { "unknown" }.into(),
        "union" => {
            // SQLite: what a declared column stores, or a number SQLite computes as an INTEGER or a REAL.
            if let (true, Some(declared)) = (dialect == "sqlite", label(t)) {
                return sqlite_declared_kind(declared);
            }
            if dialect == "postgres" && (name == "inet" || name == "cidr") {
                return "text".into();
            }
            either(items(&t["members"]).iter().map(|m| column_kind(m, dialect)).collect())
        }
        "tagged_union" => "unknown".into(),
        "struct" => {
            if dialect != "postgres" {
                return "unknown".into();
            }
            if let Some(shape) = geometric(t) {
                return shape.into();
            }
            let fields: Vec<String> = items(&t["fields"])
                .iter()
                .map(|f| {
                    let kind = column_kind(&f["type"], dialect);
                    let delim = if self::kind(&f["type"]) == "array" && kind.ends_with("[]") { delimiter_of(&f["type"]["element"]["type"]) } else { None };
                    let name = f["name"].clone();
                    Value::Array(match delim {
                        None => vec![name, Value::String(kind)],
                        Some(d) => vec![name, Value::String(kind), Value::String(d.into())],
                    })
                    .to_string()
                })
                .collect();
            format!("({})", fields.join(","))
        }
        "text" => "text".into(),
        "enum" => {
            let labels = items(&t["values"]);
            if dialect == "mysql" && name == "enum" && kind(&t["type"]) == "text" && !labels.iter().any(|v| v["value"].as_str() == Some("")) {
                "enum".into()
            } else {
                column_kind(&t["type"], dialect)
            }
        }
        k @ ("range" | "multirange") => {
            if dialect == "postgres" {
                format!("{k}:{}", column_kind(&t["subtype"], dialect))
            } else {
                "text".into()
            }
        }
        "geometry" => match dialect {
            "postgres" => "text".into(),
            "mysql" => "geometry".into(),
            _ => "unknown".into(),
        },
        "opaque" => {
            if dialect == "postgres" && name == "money" {
                return "money".into();
            }
            if dialect == "postgres" { "text" } else { "unknown" }.into()
        }
        "unknown" | "tuple" | "table" | "intersection" | "null" => "unknown".into(),
        _ => "text".into(),
    }
}

/// The TypeScript type of a scalar kind, `a|b` as the union of both; `unknown` where any part is.
fn kind_type(kind: &str) -> String {
    let parts: Vec<String> = kind.split('|').map(|k| ts_type(k).unwrap_or_else(|| "unknown".into())).collect();
    if parts.iter().any(|p| p == "unknown") {
        return "unknown".into();
    }
    dedupe(parts).join(" | ")
}

fn dedupe(parts: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for part in parts {
        if !kept.contains(&part) {
            kept.push(part);
        }
    }
    kept
}

// ---------------------------------------------------------------------------------------------
// The project file's `js.codecs` (src/codecs.ts).

fn codec_type_of(codec: &str) -> &'static str {
    match codec {
        "decimal:number" => "number",
        "decimal:decimal.js" => r#"import("decimal.js").Decimal"#,
        "decimal:bignumber.js" => r#"import("bignumber.js").BigNumber"#,
        "decimal:big.js" => r#"import("big.js").Big"#,
        "bytes:string" => "string",
        "date" => "Temporal.PlainDate",
        "time" => "Temporal.PlainTime",
        "datetime" => "Temporal.PlainDateTime",
        "instant" => "Temporal.Instant",
        "duration" => "Temporal.Duration",
        "set:array" => "string[]",
        "vector:float32" => "Float32Array",
        "vector:float16" => "Float16Array",
        "geometry" => r#"import("geojson").Geometry & { srid: number }"#,
        "inet:ip-address" => r#"import("ip-address").Address4 | import("ip-address").Address6"#,
        _ => "unknown",
    }
}

static DECLARED_SIZE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\(.*$").unwrap());

/// The family of a value of type `t` that is no array, struct, map or tagged union.
fn family(t: &Value, dialect: &str) -> Option<&'static str> {
    if kind(t) == "duration" {
        return Some("duration");
    }
    let name = bare_name(t);
    if kind(t) == "array" && (dialect == "postgres" && (name == "vector" || name == "halfvec") || dialect == "mysql" && name == "vector") {
        return Some("vector");
    }
    if dialect == "postgres" && kind(t) == "union" && (name == "inet" || name == "cidr") {
        return Some("inet");
    }
    if kind(t) == "geometry" && dialect != "sqlite" {
        return Some("geometry");
    }
    match column_kind(t, dialect).as_str() {
        "decimal" | "money" => Some("decimal"),
        "bytes" => Some("bytes"),
        "date" => Some("date"),
        "timestamp" => Some("datetime"),
        "timestamptz" => (kind(t) != "zoned_datetime").then_some("instant"),
        "text" => (kind(t) == "time" && !truthy(t.get("offset"))).then_some("time"),
        "set" => Some("set"),
        _ if dialect == "sqlite" && kind(t) == "union" => match DECLARED_SIZE.replace(name.trim(), "").to_uppercase().as_str() {
            "DATE" => Some("date"),
            "DATETIME" | "TIMESTAMP" => Some("datetime"),
            "TIME" => Some("time"),
            _ => None,
        },
        _ => None,
    }
}

/// The codec a value of type `t` takes itself, where the project's codecs give its family one other than the default.
fn leaf_codec(t: &Value, dialect: &str, codecs: &Value) -> Option<String> {
    let f = family(t, dialect)?;
    let v = codecs.get(f).and_then(Value::as_str);
    match f {
        "set" => (v == Some("Array")).then(|| "set:array".into()),
        "vector" => {
            if v != Some("TypedArray") {
                return None;
            }
            let element = &t["element"]["type"];
            Some(if kind(element) == "float" && element["exponent_bits"].as_f64() == Some(5.0) { "vector:float16" } else { "vector:float32" }.into())
        }
        "decimal" => v.filter(|v| *v != "string").map(|v| format!("decimal:{v}")),
        "inet" => (v == Some("ip-address")).then(|| "inet:ip-address".into()),
        "bytes" => (v == Some("string")).then(|| "bytes:string".into()),
        f => v.filter(|v| *v != "string").map(|_| f.into()),
    }
}

fn geojson_type(subtype: &str) -> Option<&'static str> {
    Some(match subtype {
        "point" => "Point",
        "linestring" => "LineString",
        "polygon" => "Polygon",
        "multipoint" => "MultiPoint",
        "multilinestring" => "MultiLineString",
        "multipolygon" => "MultiPolygon",
        "geometrycollection" => "GeometryCollection",
        _ => return None,
    })
}

static UNION_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[|&]").unwrap());

/// The type a codec gives a value of `t`.
fn codec_type(codec: &str, t: &Value, member: &dyn Fn(&Value) -> String, input: bool) -> String {
    if codec == "geometry" {
        let shape = if kind(t) == "geometry" { t["subtype"].as_str().and_then(geojson_type) } else { None };
        return format!(r#"import("geojson").{} & {{ srid{}: number }}"#, shape.unwrap_or("Geometry"), if input { "?" } else { "" });
    }
    if codec == "set:array" && kind(t) == "array" && is_mysql_set(t) {
        let element = member(&t["element"]["type"]);
        return if UNION_MARK.is_match(&element) { format!("({element})[]") } else { format!("{element}[]") };
    }
    codec_type_of(codec).into()
}

// ---------------------------------------------------------------------------------------------
// The renderer (src/render.ts).

const JSON_TYPE: &str = "Json";
const JSON_INPUT_TYPE: &str = "JsonInput";
const NO_CODECS: &Value = &Value::Null;

/// The form Postgres writes `t` in inside a JSON document, of those `json_image` names
/// (src/schema.ts, `postgresInJson`): a built-in structure is known by its own label, which a type
/// of another schema never has, and any other type, pgvector's among them, by its name in whatever
/// schema its extension was created in.
fn postgres_json_image(t: &Value) -> Option<&'static str> {
    let name = bare_name(t);
    let structure = matches!(kind(t), "array" | "struct" | "union") && !(kind(t) == "array" && matches!(name.as_str(), "vector" | "halfvec" | "sparsevec"));
    if structure { label(t).and_then(json_image) } else { json_image(&name) }
}

/// What Postgres writes inside a JSON document for a type whose form there its kind does not say:
/// the text of each type core's schema writer names, and an hstore's, a geometry's and an unread
/// row's own forms.
fn json_image(name: &str) -> Option<&'static str> {
    Some(match name {
        "hstore" => "Record<string, string | null>",
        "geometry" => "{ type: string; [key: string]: Json | undefined }",
        "record" => "Record<string, Json>",
        _ if reylo_engine_core::schema::POSTGRES_TEXT_IN_JSON.contains(&name) => "string",
        _ => return None,
    })
}

fn or_null(t: &str) -> String {
    if t == JSON_TYPE || t == JSON_INPUT_TYPE {
        t.into()
    } else {
        format!("{t} | null")
    }
}

static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").unwrap());

fn ident(name: &str) -> String {
    if IDENT.is_match(name) {
        name.into()
    } else {
        js_string(name)
    }
}

/// A string as `JSON.stringify` writes it.
fn js_string(s: &str) -> String {
    Value::String(s.into()).to_string()
}

/// A JSON number as JavaScript's `String()` writes it.
fn js_number(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f.fract() == 0.0 && f.abs() < 1e21 {
        return format!("{}", f as i128);
    }
    let text = n.to_string();
    match text.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => text,
    }
}

fn json_image_of(t: &Value, dialect: &str, in_union: bool) -> String {
    let name = bare_name(t);
    if is_mysql_set(t) {
        return if dialect == "sqlite" { JSON_TYPE } else { "string" }.into();
    }
    match kind(t) {
        "boolean" => "boolean".into(),
        "enum" => enum_literal(t, &json_image_of(&t["type"], dialect, in_union)),
        "tuple" => tuple_of(t, &|e| json_image_of(e, dialect, false)),
        "integer" => if dialect == "postgres" && name == "oid" { "string" } else { "number" }.into(),
        "float" | "decimal" => {
            if dialect == "sqlite" || dialect == "postgres" && !in_union {
                return "number".into();
            }
            let mut parts = vec!["number".to_string()];
            if truthy(t.get("nan")) {
                parts.push("\"NaN\"".into());
            }
            if truthy(t.get("infinity")) {
                parts.push("\"Infinity\"".into());
                parts.push("\"-Infinity\"".into());
            }
            parts.join(" | ")
        }
        "text" => "string".into(),
        "union" => {
            // A SQLite column declared BOOLEAN holds 0 and 1, which SQLite writes as numbers.
            if dialect == "sqlite" {
                return if BOOLEAN.is_match(&name) {
                    "number".into()
                } else if name.to_uppercase() == "NUMERIC" {
                    "number | string".into()
                } else if items(&t["members"]).iter().all(|m| matches!(m["kind"].as_str(), Some("integer" | "float"))) {
                    // An INTEGER that may be a REAL, which SQLite keeps outside a STRICT table.
                    "number".into()
                } else {
                    JSON_TYPE.into()
                };
            }
            if dialect == "postgres" {
                if let Some(image) = postgres_json_image(t) {
                    return image.into();
                }
            }
            let parts: Vec<String> = items(&t["members"]).iter().map(|m| json_image_of(m, dialect, true)).collect();
            if parts.iter().any(|p| p == JSON_TYPE) {
                return JSON_TYPE.into();
            }
            dedupe(parts.iter().flat_map(|p| p.split(" | ").map(String::from)).collect()).join(" | ")
        }
        "bytes" | "uuid" | "date" | "time" | "datetime" | "instant" | "duration" | "range" | "multirange" | "xml" => if dialect == "sqlite" { JSON_TYPE } else { "string" }.into(),
        _ => (if dialect == "postgres" { postgres_json_image(t) } else { None }).unwrap_or(JSON_TYPE).into(),
    }
}

static NESTED_ALREADY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\(.*\)\[\]|NestedArray<.*>)(?:\[\])*$").unwrap());

fn range_of(bound: &str, input: bool) -> String {
    format!(r#"{{ empty: true }} | {{ lower?: {bound}; upper?: {bound}; bounds{}: "[)" | "[]" | "(]" | "()" }}"#, if input { "?" } else { "" })
}

/// The TypeScript of a value of type `t` (src/render.ts, `renderType`).
fn render_type(t: &Value, in_json: bool, dialect: &str, codecs: &Value) -> String {
    let name = bare_name(t);
    if !in_json {
        if let Some(codec) = leaf_codec(t, dialect, codecs) {
            return codec_type(&codec, t, &|e| render_type(e, false, dialect, NO_CODECS), false);
        }
    }
    match kind(t) {
        "enum" => return enum_literal(t, &render_type(&t["type"], in_json, dialect, codecs)),
        "tuple" => return tuple_of(t, &|e| render_type(e, in_json, dialect, codecs)),
        "array" => {
            // A SET is its text inside a document, but on SQLite, which has none, any JSON (src/schema.ts, `inDocument`).
            if is_mysql_set(t) {
                return if !in_json && dialect == "mysql" { format!("Set<{}>", render_type(&t["element"]["type"], false, dialect, codecs)) } else if in_json && dialect == "sqlite" { JSON_TYPE.into() } else { "string".into() };
            }
            if in_json && dialect == "postgres" {
                if let Some(image) = postgres_json_image(t) {
                    return image.into();
                }
            }
            if in_json && dialect == "mysql" && name == "bit" {
                return "string".into();
            }
            if !in_json {
                let kind = column_kind(t, dialect);
                if !kind.ends_with("[]") {
                    return kind_type(&kind);
                }
            }
            let item = render_type(&t["element"]["type"], in_json, dialect, codecs);
            let inner = if is_true(&t["element"]["nullable"]) { or_null(&item) } else { item };
            if nests_any_deep(t) {
                return format!("NestedArray<{inner}>");
            }
            return if UNION_MARK.is_match(&inner) && !NESTED_ALREADY.is_match(&inner) { format!("({inner})[]") } else { format!("{inner}[]") };
        }
        "struct" => {
            if in_json {
                return match postgres_json_image(t) {
                    Some(image) if dialect == "postgres" && name != "record" => image.into(),
                    _ => render_fields(items(&t["fields"]), dialect, t.get("extra")),
                };
            }
            if dialect == "postgres" || dialect == "clickhouse" {
                let fields = items(&t["fields"]);
                if fields.is_empty() {
                    return "Record<never, never>".into();
                }
                let each: Vec<String> = last_of_each_name(fields)
                    .into_iter()
                    .map(|f| {
                        let ty = render_type(&f["type"], false, dialect, codecs);
                        format!("{}: {}", ident(f["name"].as_str().unwrap_or_default()), if is_true(&f["nullable"]) { or_null(&ty) } else { ty })
                    })
                    .collect();
                return format!("{{ {} }}", each.join("; "));
            }
        }
        "json" => {
            if let Some(content) = t.get("content") {
                let rendered = render_type(&content["type"], true, dialect, NO_CODECS);
                return if is_true(&content["nullable"]) { or_null(&rendered) } else { rendered };
            }
            return match t.get("schema") {
                None => JSON_TYPE.into(),
                Some(_) => schema_type(&schema_of(t), false),
            };
        }
        "tagged_union" if !in_json => {
            let alternatives = items(&t["alternatives"]);
            if alternatives.is_empty() {
                return "never".into();
            }
            return alternatives
                .iter()
                .map(|a| {
                    let ty = render_type(&a["type"], false, dialect, codecs);
                    format!("{{ tag: {}; value: {} }}", js_string(a["tag"].as_str().unwrap_or_default()), if is_true(&a["nullable"]) { or_null(&ty) } else { ty })
                })
                .collect::<Vec<_>>()
                .join(" | ");
        }
        k @ ("range" | "multirange") if !in_json && dialect == "postgres" => {
            let range = range_of(&render_type(&t["subtype"], false, dialect, codecs), false);
            return if k == "range" { range } else { format!("({range})[]") };
        }
        "unknown" => return if in_json { JSON_TYPE } else { "unknown" }.into(),
        _ => {}
    }
    if in_json {
        json_image_of(t, dialect, false)
    } else {
        kind_type(&column_kind(t, dialect))
    }
}

/// A JSON type's JSON Schema in the engine's canonical form (src/engine-type.ts, `schemaOf`).
fn schema_of(t: &Value) -> Value {
    let mut keywords = t["schema"].as_object().cloned().unwrap_or_default();
    keywords.remove("$schema");
    if keywords.is_empty() {
        return Value::Bool(true);
    }
    if keywords.len() == 1 && keywords.get("not").and_then(Value::as_object).is_some_and(Map::is_empty) {
        return Value::Bool(false);
    }
    Value::Object(keywords)
}

/// The TypeScript type of a JSON value the engine's canonical JSON Schema describes.
fn schema_type(schema: &Value, beside_objects: bool) -> String {
    if schema == &Value::Bool(false) {
        return "never".into();
    }
    let Some(m) = schema.as_object() else { return JSON_TYPE.into() };
    if let Some(any_of) = m.get("anyOf").and_then(Value::as_array) {
        let shared = beside_objects || object_alternatives(schema) > 1;
        return union(any_of.iter().map(|s| schema_type(s, shared)).collect());
    }
    if let Some(values) = m.get("enum").and_then(Value::as_array) {
        return union(values.iter().map(literal_type).collect());
    }
    if m.contains_key("not") {
        return "never".into();
    }
    let kinds = kinds_of(m);
    if kinds.is_empty() {
        return JSON_TYPE.into();
    }
    union(kinds.iter().map(|k| schema_kind_type(k, m, beside_objects)).collect())
}

fn kinds_of(m: &Map<String, Value>) -> Vec<String> {
    match m.get("type") {
        Some(Value::String(k)) => vec![k.clone()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(String::from).collect(),
        _ => Vec::new(),
    }
}

fn object_alternatives(schema: &Value) -> usize {
    let Some(m) = schema.as_object() else { return 0 };
    if let Some(any_of) = m.get("anyOf").and_then(Value::as_array) {
        return any_of.iter().map(object_alternatives).sum();
    }
    if let Some(values) = m.get("enum").and_then(Value::as_array) {
        return values.iter().filter(|v| v.is_object()).count();
    }
    usize::from(kinds_of(m).iter().any(|k| k == "object"))
}

fn schema_kind_type(kind: &str, m: &Map<String, Value>, beside_objects: bool) -> String {
    match kind {
        "string" => "string".into(),
        "integer" | "number" => "number".into(),
        "boolean" => "boolean".into(),
        "null" => "null".into(),
        "array" => {
            let prefix: Vec<String> = m.get("prefixItems").and_then(Value::as_array).map(|p| p.iter().map(|s| schema_type(s, false)).collect()).unwrap_or_default();
            let rest = m.get("items").map_or_else(|| JSON_TYPE.to_string(), |s| schema_type(s, false));
            if prefix.is_empty() {
                return if rest == "never" { "[]".into() } else { format!("{}[]", grouped(&rest)) };
            }
            let min = m.get("minItems").and_then(Value::as_f64).unwrap_or(0.0);
            let mut items: Vec<String> = prefix.iter().enumerate().map(|(i, item)| if (i as f64) < min { item.clone() } else { format!("{}?", grouped(item)) }).collect();
            if rest != "never" {
                items.push(format!("...{}[]", grouped(&rest)));
            }
            format!("[{}]", items.join(", "))
        }
        "object" => {
            let empty = Map::new();
            let properties = m.get("properties").and_then(Value::as_object).unwrap_or(&empty);
            let required = m.get("required").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            let mut fields: Vec<String> = js_entries(properties).into_iter().map(|(key, value)| format!("{}{}: {}", ident(key), if required.iter().any(|r| r.as_str() == Some(key)) { "" } else { "?" }, schema_type(value, false))).collect();
            let additional = m.get("additionalProperties");
            let rest = match additional {
                None | Some(Value::Bool(false)) => "never".to_string(),
                Some(s) => schema_type(s, false),
            };
            let open = additional.is_none() || rest == JSON_TYPE;
            let signature = if fields.is_empty() { rest != "never" } else { (rest != "never" && rest != JSON_TYPE) || (open && beside_objects) };
            if signature {
                let of = if fields.is_empty() { rest.clone() } else { format!("{JSON_TYPE} | undefined") };
                fields.push(format!("[key: string]: {of}"));
            }
            if !fields.is_empty() {
                return format!("{{ {} }}", fields.join("; "));
            }
            if additional.is_none() { format!("Record<string, {JSON_TYPE}>") } else { "Record<never, never>".into() }
        }
        _ => JSON_TYPE.into(),
    }
}

/// An object's entries in the order JavaScript's `Object.entries` gives them: each key that is an
/// array index (a canonical integer below 2^32 - 1) first, ascending, then the rest as written.
fn js_entries(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let index = |key: &str| key.parse::<u32>().ok().filter(|n| *n != u32::MAX && n.to_string() == key);
    let mut entries: Vec<(&String, &Value)> = map.iter().filter(|(k, _)| index(k).is_some()).collect();
    entries.sort_by_key(|(k, _)| index(k));
    entries.extend(map.iter().filter(|(k, _)| index(k).is_none()));
    entries
}

/// The TypeScript literal type of one JSON value.
fn literal_type(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => js_string(s),
        Value::Number(n) => js_number(n),
        Value::Bool(b) => b.to_string(),
        Value::Array(items) => format!("[{}]", items.iter().map(literal_type).collect::<Vec<_>>().join(", ")),
        Value::Object(map) if map.is_empty() => "Record<never, never>".into(),
        Value::Object(map) => format!("{{ {} }}", js_entries(map).into_iter().map(|(k, x)| format!("{}: {}", ident(k), literal_type(x))).collect::<Vec<_>>().join("; ")),
    }
}

/// A union of JSON types: `Json` absorbs the rest, and `never` adds nothing.
fn union(parts: Vec<String>) -> String {
    let kept = dedupe(parts);
    if kept.iter().any(|p| p == JSON_TYPE) {
        return JSON_TYPE.into();
    }
    let real: Vec<String> = kept.into_iter().filter(|p| p != "never").collect();
    if real.is_empty() {
        "never".into()
    } else {
        real.join(" | ")
    }
}

/// An enum's values as TypeScript's literal types, where its base renders as the primitive they are values of.
fn enum_literal(t: &Value, base: &str) -> String {
    let values: Vec<&str> = items(&t["values"]).iter().map(|v| v["value"].as_str().unwrap_or_default()).collect();
    match (kind(&t["type"]), base) {
        ("boolean", "boolean") | ("integer", "number") | ("float", "number") => values.join(" | "),
        ("integer", "bigint") => values.iter().map(|v| format!("{v}n")).collect::<Vec<_>>().join(" | "),
        ("text", "string") => values.iter().map(|v| js_string(v)).collect::<Vec<_>>().join(" | "),
        _ => base.into(),
    }
}

/// A tuple's TypeScript: its elements, then its optional ones with `?`, then its rest.
fn tuple_of(t: &Value, render: &dyn Fn(&Value) -> String) -> String {
    let slot = |s: &Value| {
        let ty = render(&s["type"]);
        if is_true(&s["nullable"]) {
            or_null(&ty)
        } else {
            ty
        }
    };
    let mut parts: Vec<String> = items(&t["elements"]).iter().map(slot).collect();
    parts.extend(items(&t["optional_elements"]).iter().map(|s| format!("{}?", grouped(&slot(s)))));
    if let Some(rest) = t.get("rest") {
        parts.push(format!("...{}[]", grouped(&slot(rest))));
    }
    format!("[{}]", parts.join(", "))
}

/// A type in parentheses where it is a union at its own top level.
fn grouped(ts: &str) -> String {
    let mut depth = 0i32;
    let mut quoted = false;
    let mut chars = ts.chars();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '\\' {
                chars.next();
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
        } else if "{([<".contains(c) {
            depth += 1;
        } else if "})]>".contains(c) {
            depth -= 1;
        } else if c == '|' && depth == 0 {
            return format!("({ts})");
        }
    }
    ts.into()
}

/// The last of each name, as an object keeps the last of two keys of one name.
fn last_of_each_name(items: &[Value]) -> Vec<&Value> {
    let last: HashMap<String, usize> = items.iter().enumerate().map(|(i, item)| (item["name"].to_string(), i)).collect();
    items.iter().enumerate().filter(|(i, item)| last[&item["name"].to_string()] == *i).map(|(_, item)| item).collect()
}

/// An object type of JSON-encoded fields, each nullable as the engine says.
fn render_fields(fields: &[Value], dialect: &str, extra: Option<&Value>) -> String {
    let fields = last_of_each_name(fields);
    let slot = |s: &Value| {
        let ty = render_type(&s["type"], true, dialect, NO_CODECS);
        if is_true(&s["nullable"]) {
            or_null(&ty)
        } else {
            ty
        }
    };
    let each: Vec<(&Value, String)> = fields.iter().map(|f| (*f, slot(f))).collect();
    let name = |f: &Value| ident(f["name"].as_str().unwrap_or_default());
    if let Some(extra) = extra {
        let mut all = vec![slot(extra)];
        all.extend(each.iter().map(|(f, t)| if is_true(&f["optional"]) { format!("{t} | undefined") } else { t.clone() }));
        let mut members: Vec<String> = each.iter().map(|(f, t)| format!("{}{}: {t}", name(f), if is_true(&f["optional"]) { "?" } else { "" })).collect();
        members.push(format!("[key: string]: {}", dedupe(all).join(" | ")));
        return format!("{{ {} }}", members.join("; "));
    }
    if each.is_empty() {
        return "Record<never, never>".into();
    }
    format!("{{ {} }}", each.iter().map(|(f, t)| format!("{}: {t}", name(f))).collect::<Vec<_>>().join("; "))
}

/// The type a parameter accepts (src/render.ts, `renderParamType`).
fn render_param_type(t: &Value, dialect: &str, in_row: bool, members: Option<&Value>, codecs: &Value) -> String {
    if let Some(codec) = leaf_codec(t, dialect, codecs) {
        return codec_type(&codec, t, &|e| render_param_type(e, dialect, true, None, NO_CODECS), true);
    }
    let kind = column_kind(t, dialect);
    // A ClickHouse parameter's type says all of it: whether an array's elements or a tuple's may be
    // NULL, and a tuple's names. A wide integer takes a BigInt, or a number that is whole.
    if dialect == "clickhouse" {
        let slot = |s: &Value| {
            let ty = render_param_type(&s["type"], dialect, false, None, codecs);
            if is_true(&s["nullable"]) {
                or_null(&ty)
            } else {
                ty
            }
        };
        match self::kind(t) {
            "array" => return format!("{}[]", grouped(&slot(&t["element"]))),
            "struct" => {
                let fields = items(&t["fields"]);
                if fields.is_empty() {
                    return "Record<never, never>".into();
                }
                let each: Vec<String> = last_of_each_name(fields).into_iter().map(|f| format!("{}: {}", ident(f["name"].as_str().unwrap_or_default()), slot(f))).collect();
                return format!("{{ {} }}", each.join("; "));
            }
            "tuple" => return format!("[{}]", items(&t["elements"]).iter().map(slot).collect::<Vec<_>>().join(", ")),
            _ => {}
        }
        if kind == "int8" {
            return "number | bigint".into();
        }
    }
    if self::kind(t) == "json" {
        return match t.get("schema") {
            None => JSON_INPUT_TYPE.into(),
            Some(_) => schema_type(&schema_of(t), false),
        };
    }
    if members.is_some() {
        let record = match self::kind(t) {
            "struct" => Some(t),
            "array" if self::kind(&t["element"]["type"]) == "struct" => Some(&t["element"]["type"]),
            _ => None,
        };
        if let Some(record) = record {
            let fields: Vec<String> = items(&record["fields"])
                .iter()
                .map(|f| {
                    let ty = render_param_type(&f["type"], dialect, true, None, codecs);
                    format!("{}{}: {}", ident(f["name"].as_str().unwrap_or_default()), if is_true(&f["optional"]) { "?" } else { "" }, if is_true(&f["nullable"]) { or_null(&ty) } else { ty })
                })
                .collect();
            let row = if fields.is_empty() { "Record<never, never>".to_string() } else { format!("{{ {} }}", fields.join("; ")) };
            return if self::kind(t) == "struct" { row } else { format!("readonly {row}[]") };
        }
    }
    if dialect == "postgres" && matches!(self::kind(t), "range" | "multirange") {
        let range = range_of(&render_param_type(&t["subtype"], dialect, false, None, codecs), true);
        return if self::kind(t) == "range" { range } else { format!("({range})[]") };
    }
    if dialect == "postgres" && self::kind(t) == "struct" && members.is_none() && kind != "text" {
        if !is_row_kind(&kind) {
            return render_type(t, false, dialect, codecs);
        }
        let fields = items(&t["fields"]);
        if fields.is_empty() {
            return "Record<never, never>".into();
        }
        let each: Vec<String> = last_of_each_name(fields)
            .into_iter()
            .map(|f| {
                let ty = render_param_type(&f["type"], dialect, true, None, codecs);
                format!("{}: {}", ident(f["name"].as_str().unwrap_or_default()), if is_true(&f["nullable"]) { or_null(&ty) } else { ty })
            })
            .collect();
        return format!("{{ {} }}", each.join("; "));
    }
    if dialect != "sqlite" {
        if kind == "int8" {
            return "number | bigint | string".into();
        }
        if kind.split('|').all(|part| matches!(part, "timestamp" | "timestamptz" | "date")) {
            return "string".into();
        }
    }
    if dialect == "sqlite" {
        match kind.as_str() {
            "int" => return "number".into(),
            "int8" => return "number | bigint".into(),
            "int|float|text" => return "number | string | boolean".into(),
            "bool" => return "boolean | number".into(),
            _ => {}
        }
    }
    if dialect == "mysql" && kind == "bool" && self::kind(t) != "boolean" {
        return "boolean | number".into();
    }
    if kind == "bytes" {
        return "Uint8Array".into();
    }
    if kind == "decimal" && dialect != "sqlite" {
        return "string".into();
    }
    if self::kind(t) == "array" {
        if dialect == "postgres" && kind == "text" && bare_name(&t["element"]["type"]) == "record" {
            return "string | string[]".into();
        }
        if kind.ends_with("[]") {
            let element = render_param_type(&t["element"]["type"], dialect, in_row, None, codecs);
            let inner = if in_row { or_null(&element) } else { element };
            if nests_any_deep(t) {
                return format!("NestedArray<{inner}>");
            }
            return if UNION_MARK.is_match(&inner) { format!("({inner})[]") } else { format!("{inner}[]") };
        }
    }
    render_type(t, false, dialect, codecs)
}

// ---------------------------------------------------------------------------------------------
// A statement's module types (src/module-types.ts, src/doc-comment.ts).

struct ModuleTypes {
    params: String,
    row: String,
    cardinality: String,
    /// Whether the engine analyzed the statement: a row of `unknown` is then the element of its
    /// cardinality, as for a column of no name whose type is unknown, and otherwise the whole result.
    analyzed: bool,
}

/// The fence a Markdown text opens and never closes.
fn open_fence(text: &str) -> Option<String> {
    static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ {0,3}(`{3,}|~{3,})").unwrap());
    let mut open: Option<String> = None;
    for line in text.split('\n') {
        let Some(m) = FENCE.captures(line) else { continue };
        let fence = m.get(1).unwrap().as_str();
        match &open {
            None => open = Some(fence.to_string()),
            Some(o) if fence.as_bytes()[0] == o.as_bytes()[0] && fence.len() >= o.len() && line[m.get(0).unwrap().end()..].trim().is_empty() => open = None,
            _ => {}
        }
    }
    open
}

/// A JSDoc comment of `prose`, indented by `indent` (src/doc-comment.ts, `docComment`, with no statement or note).
fn doc_comment(prose: &str, indent: &str) -> String {
    if prose.is_empty() {
        return String::new();
    }
    let part = match open_fence(prose) {
        Some(open) => format!("{prose}\n{open}"),
        None => prose.to_string(),
    };
    let body = part.replace("*/", "*\\/");
    let mut lines = vec![format!("{indent}/**")];
    lines.extend(body.split('\n').map(|l| if l.is_empty() { format!("{indent} *") } else { format!("{indent} * {l}") }));
    lines.push(format!("{indent} */"));
    lines.push(String::new());
    lines.join("\n")
}

/// A column's comment as the doc comment of the field that reads it.
fn field_comment(comment: Option<&str>, indent: &str) -> String {
    static TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)[ \t]+$").unwrap());
    let text = comment.unwrap_or_default().replace("\r\n", "\n").replace('\r', "\n");
    let text = TRAILING.replace_all(&text, "");
    let text = js_trim(&text);
    if text.is_empty() {
        return String::new();
    }
    if !text.contains('\n') {
        return format!("/** {} */", text.replace("*/", "*\\/"));
    }
    js_trim(&doc_comment(text, indent)).to_string()
}

/// A string without the white space and line terminators JavaScript's `trim()` removes at its ends.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// An object type of `fields`, each a field's text and its column's comment.
fn object_type(fields: &[(String, Option<&str>)]) -> String {
    let docs: Vec<String> = fields.iter().map(|(_, comment)| field_comment(*comment, "  ")).collect();
    if docs.iter().all(String::is_empty) {
        return format!("{{ {} }}", fields.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>().join("; "));
    }
    let lines: Vec<String> = fields.iter().zip(&docs).map(|((text, _), doc)| format!("{}  {text};", if doc.is_empty() { String::new() } else { format!("  {doc}\n") })).collect();
    format!("{{\n{}\n}}", lines.join("\n"))
}

/// The diagnostic code of a statement the database refused (src/engine-core.ts, `DIAGNOSTIC.refused`).
const REFUSED: u64 = 6;

fn module_types(written: &Value, dialect: &str, codecs: &Value, allow_nested_arrays: bool) -> ModuleTypes {
    let empty = Map::new();
    let domains = written.get("domains").and_then(Value::as_object).unwrap_or(&empty);
    let based = |t: &Value| if domains.is_empty() { t.clone() } else { with_bases(t, domains) };
    // A parameter nests where the project lets columns nest, so it takes back what a column of its type returns.
    let nesting = |slot: &Value, parameter: bool| {
        let ty = based(&slot["type"]);
        with_type(slot, if allow_nested_arrays { mark_nesting(&ty, parameter) } else { ty })
    };
    let params: Vec<Value> = items(&written["params"]).iter().map(|p| nesting(p, true)).collect();
    let columns: Vec<Value> = items(&written["columns"]).iter().map(|c| nesting(c, false)).collect();
    let cardinality = match written["cardinality"].as_str().unwrap_or("many") {
        "at_most_one" => "maybeOne",
        other => other,
    }
    .to_string();
    let refused = items(&written["diagnostics"]).iter().any(|d| d["code"].as_u64() == Some(REFUSED));
    if refused || dialect == "postgres" && written.get("kind").is_none_or(Value::is_null) {
        return ModuleTypes { params: "unknown".into(), row: "unknown".into(), cardinality, analyzed: false };
    }
    let column_types: Vec<(String, Option<&str>)> = last_of_each_name(&columns)
        .into_iter()
        .map(|c| {
            let ty = render_type(&c["type"], false, dialect, codecs);
            (format!("{}: {}", ident(c["name"].as_str().unwrap_or_default()), if is_true(&c["nullable"]) { or_null(&ty) } else { ty }), c.get("comment").and_then(Value::as_str))
        })
        .collect();
    let param_types: Vec<(String, Option<&str>)> = params
        .iter()
        .map(|q| {
            let members = q.get("members").filter(|m| !m.is_null());
            let rendered = render_param_type(&q["type"], dialect, false, members, codecs);
            let ty = if kind(&q["type"]) == "unknown" {
                "unknown".to_string()
            } else if is_true(&q["nullable"]) {
                or_null(&rendered)
            } else {
                rendered
            };
            let optional = is_true(&q["nullable"]) || is_true(&q["optional"]);
            (format!("{}{}: {ty}", ident(q["name"].as_str().unwrap_or_default()), if optional { "?" } else { "" }), q.get("comment").and_then(Value::as_str))
        })
        .collect();
    let row = if is_true(&written["value"]) && !columns.is_empty() {
        let ty = render_type(&columns[0]["type"], false, dialect, codecs);
        if is_true(&columns[0]["nullable"]) {
            or_null(&ty)
        } else {
            ty
        }
    } else if column_types.is_empty() {
        "never".into()
    } else {
        object_type(&column_types)
    };
    ModuleTypes { params: if param_types.is_empty() { "Record<never, never>".into() } else { object_type(&param_types) }, row, cardinality, analyzed: true }
}

fn with_type(slot: &Value, ty: Value) -> Value {
    let mut out = slot.clone();
    out["type"] = ty;
    out
}

// ---------------------------------------------------------------------------------------------
// The layout (src/type-format.ts).

enum Node {
    Name(String),
    Object(Vec<Member>),
    Union(Vec<Node>),
    Array(Box<Node>),
    Tuple(Vec<Item>),
    Readonly(Box<Node>),
}

struct Member {
    key: String,
    ty: Node,
    doc: Option<String>,
}

struct Item {
    ty: Node,
    optional: bool,
    rest: bool,
}

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"/\*\*[\s\S]*?\*/|"(?:[^"\\]|\\.)*"|-?[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?|[A-Za-z_$][A-Za-z0-9_$]*(?:<[^<>]*>)?|\.\.\.|\[\]|[{}()\[\]|;:?,]"#).unwrap());

/// The length JavaScript gives a string, in UTF-16 units, which the layout counts columns in.
fn width(s: &str) -> usize {
    s.encode_utf16().count()
}

struct Parser<'a> {
    tokens: Vec<&'a str>,
    at: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.at).copied()
    }

    fn take(&mut self, expected: Option<&str>) -> Result<&'a str, ()> {
        let token = self.tokens.get(self.at).copied();
        self.at += 1;
        match token {
            Some(t) if expected.is_none_or(|e| e == t) => Ok(t),
            _ => Err(()),
        }
    }

    fn union(&mut self) -> Result<Node, ()> {
        let mut of = vec![self.postfix()?];
        while self.peek() == Some("|") {
            self.take(Some("|"))?;
            of.push(self.postfix()?);
        }
        Ok(if of.len() == 1 { of.pop().unwrap() } else { Node::Union(of) })
    }

    fn postfix(&mut self) -> Result<Node, ()> {
        if self.peek() == Some("readonly") {
            self.take(Some("readonly"))?;
            return Ok(Node::Readonly(Box::new(self.postfix()?)));
        }
        let mut node = self.primary()?;
        while self.peek() == Some("[]") {
            self.take(Some("[]"))?;
            node = Node::Array(Box::new(node));
        }
        Ok(node)
    }

    fn primary(&mut self) -> Result<Node, ()> {
        match self.peek() {
            Some("(") => {
                self.take(Some("("))?;
                let inner = self.union()?;
                self.take(Some(")"))?;
                Ok(inner)
            }
            Some("{") => self.object(),
            Some("[]") => {
                self.take(Some("[]"))?;
                Ok(Node::Tuple(Vec::new()))
            }
            Some("[") => self.tuple(),
            _ => Ok(Node::Name(self.take(None)?.to_string())),
        }
    }

    fn tuple(&mut self) -> Result<Node, ()> {
        self.take(Some("["))?;
        let mut items = Vec::new();
        while self.peek() != Some("]") {
            let rest = self.peek() == Some("...");
            if rest {
                self.take(Some("..."))?;
            }
            let ty = self.union()?;
            let optional = self.peek() == Some("?");
            if optional {
                self.take(Some("?"))?;
            }
            items.push(Item { ty, optional, rest });
            if self.peek() == Some(",") {
                self.take(Some(","))?;
            } else if self.peek() != Some("]") {
                self.take(Some("]"))?;
            }
        }
        self.take(Some("]"))?;
        Ok(Node::Tuple(items))
    }

    fn object(&mut self) -> Result<Node, ()> {
        self.take(Some("{"))?;
        let mut members = Vec::new();
        while self.peek() != Some("}") {
            let doc = if self.peek().is_some_and(|t| t.starts_with("/**")) {
                Some(self.take(None)?.split('\n').enumerate().map(|(i, line)| if i > 0 { format!(" {}", line.trim()) } else { line.trim().to_string() }).collect::<Vec<_>>().join("\n"))
            } else {
                None
            };
            let mut key = if self.peek() == Some("[") {
                self.take(Some("["))?;
                let name = self.take(None)?;
                self.take(Some(":"))?;
                let of = self.take(None)?;
                self.take(Some("]"))?;
                format!("[{name}: {of}]")
            } else {
                self.take(None)?.to_string()
            };
            if self.peek() == Some("?") {
                key.push_str(self.take(Some("?"))?);
            }
            self.take(Some(":"))?;
            members.push(Member { key, ty: self.union()?, doc });
            if self.peek() == Some(";") {
                self.take(Some(";"))?;
            }
        }
        self.take(Some("}"))?;
        Ok(Node::Object(members))
    }
}

/// The type's text as a tree; `None` on text outside the forms the renderer writes.
fn parse_type(text: &str) -> Option<Node> {
    if !TOKEN.replace_all(text, "").trim().is_empty() {
        return None;
    }
    let mut parser = Parser { tokens: TOKEN.find_iter(text).map(|m| m.as_str()).collect(), at: 0 };
    let node = parser.union().ok()?;
    (parser.at == parser.tokens.len()).then_some(node)
}

fn one_line_doc(doc: &str) -> String {
    static END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\*/$").unwrap());
    static STARS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\n[ \t]*\*[ \t]*)+").unwrap());
    static START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/\*\*\s*").unwrap());
    let doc = END.replace(doc, " */");
    let doc = STARS.replace_all(&doc, " ");
    START.replace(&doc, "/** ").into_owned()
}

fn has_doc(node: &Node) -> bool {
    match node {
        Node::Name(_) => false,
        Node::Object(members) => members.iter().any(|m| m.doc.is_some() || has_doc(&m.ty)),
        Node::Union(of) => of.iter().any(has_doc),
        Node::Tuple(items) => items.iter().any(|i| has_doc(&i.ty)),
        Node::Array(of) | Node::Readonly(of) => has_doc(of),
    }
}

fn loose(node: &Node) -> bool {
    matches!(node, Node::Union(_) | Node::Readonly(_))
}

fn around(item: &Item) -> (String, String) {
    let wrap = item.optional && loose(&item.ty);
    (format!("{}{}", if item.rest { "..." } else { "" }, if wrap { "(" } else { "" }), format!("{}{}", if wrap { ")" } else { "" }, if item.optional { "?" } else { "" }))
}

fn flat(node: &Node) -> String {
    match node {
        Node::Name(text) => text.clone(),
        Node::Object(members) if members.is_empty() => "{}".into(),
        Node::Object(members) => format!("{{ {} }}", members.iter().map(|m| format!("{}{}: {}", m.doc.as_deref().map(|d| format!("{} ", one_line_doc(d))).unwrap_or_default(), m.key, flat(&m.ty))).collect::<Vec<_>>().join("; ")),
        Node::Union(of) => of.iter().map(flat).collect::<Vec<_>>().join(" | "),
        Node::Array(of) => {
            if loose(of) {
                format!("({})[]", flat(of))
            } else {
                format!("{}[]", flat(of))
            }
        }
        Node::Tuple(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| {
                    let (before, after) = around(item);
                    format!("{before}{}{after}", flat(&item.ty))
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Node::Readonly(of) => format!("readonly {}", flat(of)),
    }
}

fn fits(node: &Node, lead: usize, width_: usize, reserve: usize) -> bool {
    lead + width(&flat(node)) + reserve <= width_ && !has_doc(node)
}

fn hugs(node: &Node) -> bool {
    matches!(node, Node::Union(of) if of.iter().filter(|n| !matches!(n, Node::Name(t) if t == "null" || t == "undefined")).count() == 1)
}

fn piped(node: &Node, lead: usize, width_: usize, reserve: usize) -> bool {
    matches!(node, Node::Union(_)) && !hugs(node) && !fits(node, lead, width_, reserve)
}

fn pipes(of: &[Node], indent: usize, width_: usize, reserve: usize) -> String {
    of.iter().enumerate().map(|(i, n)| format!("| {}", print(n, indent + 2, indent + 2, width_, if i + 1 == of.len() { reserve } else { 0 }))).collect::<Vec<_>>().join(&format!("\n{}", " ".repeat(indent)))
}

fn union_members(node: &Node) -> &[Node] {
    match node {
        Node::Union(of) => of,
        _ => std::slice::from_ref(node),
    }
}

fn print(node: &Node, indent: usize, lead: usize, width_: usize, reserve: usize) -> String {
    let one = flat(node);
    if fits(node, lead, width_, reserve) {
        return one;
    }
    let pad = " ".repeat(indent);
    match node {
        Node::Name(_) => one,
        Node::Object(members) => {
            let inner = " ".repeat(indent + 2);
            let doc = |text: &Option<String>| text.as_deref().map(|t| t.split('\n').map(|line| format!("{inner}{line}\n")).collect::<String>()).unwrap_or_default();
            let member = |m: &Member| {
                let lead = indent + 2 + width(&m.key) + 2;
                if !piped(&m.ty, lead, width_, 1) {
                    return format!("{}: {};", m.key, print(&m.ty, indent + 2, lead, width_, 1));
                }
                format!("{}:\n{inner}  {};", m.key, pipes(union_members(&m.ty), indent + 4, width_, 1))
            };
            format!("{{\n{}\n{pad}}}", members.iter().map(|m| format!("{}{inner}{}", doc(&m.doc), member(m))).collect::<Vec<_>>().join("\n"))
        }
        Node::Union(of) => {
            if !hugs(node) && lead == indent {
                return pipes(of, indent, width_, reserve);
            }
            let mut line = lead;
            let mut parts = Vec::new();
            for (i, n) in of.iter().enumerate() {
                let tail = if i + 1 < of.len() { 3 + width(&of[i + 1..].iter().map(flat).collect::<Vec<_>>().join(" | ")) + reserve } else { reserve };
                let text = print(n, indent, line, width_, tail);
                let last = text.rsplit('\n').next().unwrap_or_default();
                line = if text.contains('\n') { width(last) } else { line + width(last) } + 3;
                parts.push(text);
            }
            parts.join(" | ")
        }
        Node::Array(of) => {
            if piped(of, lead + 1, width_, reserve + 3) {
                return format!("(\n{pad}  {}\n{pad})[]", pipes(union_members(of), indent + 2, width_, 0));
            }
            if loose(of) {
                format!("({})[]", print(of, indent, lead + 1, width_, reserve + 3))
            } else {
                format!("{}[]", print(of, indent, lead, width_, reserve + 2))
            }
        }
        Node::Tuple(items) => {
            if items.is_empty() {
                return one;
            }
            let inner = " ".repeat(indent + 2);
            let lines: Vec<String> = items
                .iter()
                .map(|item| {
                    let (before, after) = around(item);
                    if before == "(" && piped(&item.ty, indent + 3, width_, after.len() + 1) {
                        return format!("{inner}(\n{inner}  {}\n{inner}{after},", pipes(union_members(&item.ty), indent + 4, width_, 0));
                    }
                    format!("{inner}{before}{}{after},", print(&item.ty, indent + 2, indent + 2 + before.len(), width_, after.len() + 1))
                })
                .collect();
            format!("[\n{}\n{pad}]", lines.join("\n"))
        }
        Node::Readonly(of) => format!("readonly {}", print(of, indent, lead + "readonly ".len(), width_, reserve)),
    }
}

/// The type's text, broken over lines where it is wider than `width_`; as it is where the layout
/// reads no form in it (a codec's `import("…")` type).
pub fn format_type(text: &str, width_: usize) -> String {
    match parse_type(text) {
        Some(node) => print(&node, 0, 0, width_, 0),
        None => text.to_string(),
    }
}

/// What `.run()` resolves to for a statement's row type and cardinality.
pub fn result_type(row: &str, cardinality: &str) -> String {
    static BRACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^{}]*\}").unwrap());
    if cardinality == "none" || row == "never" {
        return "void".into();
    }
    let outer = BRACES.replace_all(row, "");
    if cardinality == "one" {
        return row.into();
    }
    if cardinality == "maybeOne" {
        return if outer.split('|').any(|part| part.trim() == "null") { row.into() } else { format!("{row} | null") };
    }
    if outer.contains('|') {
        format!("({row})[]")
    } else {
        format!("{row}[]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn analysis(columns: Value, params: Value, cardinality: &str) -> Value {
        json!({ "kind": "select", "params": params, "columns": columns, "cardinality": cardinality, "diagnostics": [] })
    }

    #[test]
    fn a_row_and_its_parameters_read_as_the_module_declares_them() {
        let a = analysis(
            json!([{ "name": "id", "type": { "kind": "integer", "bits": 64, "label": "int8" } }, { "name": "bio", "type": { "kind": "text", "label": "text" }, "nullable": true }]),
            json!([{ "name": "id", "type": { "kind": "integer", "bits": 64, "label": "int8" } }]),
            "at_most_one",
        );
        let p = printed(&a, "postgres", &Value::Null, false);
        assert_eq!(p.text(), "type Params = { id: number | bigint | string };\ntype Returns = { id: bigint; bio: string | null } | null;\n");
    }

    #[test]
    fn the_cardinality_is_applied() {
        assert_eq!(result_type("{ a: string | null }", "many"), "{ a: string | null }[]");
        assert_eq!(result_type("string | null", "many"), "(string | null)[]");
        assert_eq!(result_type("string | null", "maybeOne"), "string | null");
        assert_eq!(result_type("string", "maybeOne"), "string | null");
        assert_eq!(result_type("never", "many"), "void");
        assert_eq!(result_type("string", "none"), "void");
    }

    #[test]
    fn codecs_give_a_family_another_type() {
        let a = analysis(json!([{ "name": "price", "type": { "kind": "decimal", "label": "numeric" } }, { "name": "at", "type": { "kind": "instant", "label": "timestamptz" } }]), json!([]), "many");
        let codecs = json!({ "decimal": "number", "instant": "Temporal.Instant" });
        assert_eq!(printed(&a, "postgres", &codecs, false).text(), "type Returns = { price: number; at: Temporal.Instant }[];\n");
        assert_eq!(printed(&a, "postgres", &Value::Null, false).text(), "type Returns = { price: string; at: string }[];\n");
    }

    #[test]
    fn an_array_of_a_type_structured_as_an_array_nests_at_any_depth() {
        let bit = json!({ "kind": "array", "size": { "exact": 3 }, "element": { "type": { "kind": "boolean" } }, "label": "bit" });
        let float8 = json!({ "kind": "float", "exponent_bits": 11, "mantissa_bits": 52, "label": "float8" });
        let point = json!({ "kind": "struct", "fields": [{ "name": "x", "type": float8 }, { "name": "y", "type": float8 }], "label": "point" });
        let lseg = json!({ "kind": "array", "size": { "exact": 2 }, "element": { "type": point }, "label": "lseg" });
        let array_of = |label: &str, ty: &Value| json!({ "kind": "array", "label": label, "element": { "nullable": true, "type": ty } });
        let a = analysis(
            json!([
                { "name": "flags", "type": array_of("_bit", &bit) },
                { "name": "segments", "type": array_of("_lseg", &lseg) },
                // `array[flags, flags]`: a level the statement builds over the column's array.
                { "name": "stacked", "type": { "kind": "array", "label": "_bit", "element": { "type": array_of("_bit", &bit) } } },
            ]),
            json!([]),
            "many",
        );
        assert_eq!(
            printed(&a, "postgres", &Value::Null, true).text(),
            "type Returns = {\n  flags: NestedArray<string | null>;\n  segments: NestedArray<{ x: number; y: number }[] | null>;\n  stacked: NestedArray<string | null>[];\n}[];\n"
        );
    }

    #[test]
    fn an_array_parameter_nests_at_any_depth_where_columns_do() {
        let int4 = json!({ "kind": "integer", "bits": 32, "label": "int4" });
        let a = analysis(json!([]), json!([{ "name": "cells", "type": { "kind": "array", "label": "_int4", "element": { "type": int4 } } }]), "none");
        assert_eq!(printed(&a, "postgres", &Value::Null, false).text(), "type Params = { cells: number[] };\ntype Returns = void;\n");
        assert_eq!(printed(&a, "postgres", &Value::Null, true).text(), "type Params = { cells: NestedArray<number> };\ntype Returns = void;\n");
        // An array of whole arrays, a domain's over `int4[]`, keeps one level outside them, which a parameter writes each of whole.
        let ints = json!({ "kind": "array", "label": "public._ints", "element": { "type": { "kind": "array", "label": "public.ints", "element": { "type": int4 } } } });
        let mut b = analysis(json!([]), json!([{ "name": "x", "type": ints }]), "none");
        b["domains"] = json!({ "public.ints": "_int4" });
        assert_eq!(printed(&b, "postgres", &Value::Null, true).text(), "type Params = { x: NestedArray<number>[] };\ntype Returns = void;\n");
        // A JSON element may be an array itself, which a parameter writes whole.
        let docs = analysis(json!([]), json!([{ "name": "docs", "type": { "kind": "array", "label": "_jsonb", "element": { "type": { "kind": "json", "label": "jsonb" } } } }]), "none");
        assert_eq!(printed(&docs, "postgres", &Value::Null, true).text(), printed(&docs, "postgres", &Value::Null, false).text());
    }

    #[test]
    fn a_wide_type_breaks_over_lines_and_a_comment_documents_its_field() {
        let a = analysis(
            json!([
                { "name": "identifier", "type": { "kind": "integer", "bits": 32, "label": "int4" }, "comment": "The user's id" },
                { "name": "email_address", "type": { "kind": "text", "label": "text" } },
            ]),
            json!([]),
            "many",
        );
        assert_eq!(printed(&a, "postgres", &Value::Null, false).returns, "{\n  /** The user's id */\n  identifier: number;\n  email_address: string;\n}[]");
        assert_eq!(format_type("{ a: string; b: number }", 10), "{\n  a: string;\n  b: number;\n}");
        assert_eq!(format_type(r#"import("geojson").Point & { srid: number }"#, 10), r#"import("geojson").Point & { srid: number }"#);
    }

    #[test]
    fn an_array_of_a_domain_over_an_array_type_holds_whole_arrays_at_each_level() {
        // `create domain ints as int4[]; create table d (x ints[] not null)`.
        let int4 = json!({ "kind": "integer", "bits": 32, "label": "int4" });
        let ints = json!({ "kind": "array", "label": "public._ints", "element": { "nullable": true, "type": { "kind": "array", "label": "public.ints", "element": { "nullable": true, "type": int4 } } } });
        let mut a = analysis(json!([{ "name": "x", "type": ints }]), json!([]), "many");
        a["domains"] = json!({ "public.ints": "_int4" });
        assert_eq!(column_kind(&with_bases(&ints, a["domains"].as_object().unwrap()), "postgres"), "{int[]}[]");
        assert_eq!(printed(&a, "postgres", &Value::Null, false).returns, "{ x: ((number | null)[] | null)[] }[]");
        assert_eq!(printed(&a, "postgres", &Value::Null, true).returns, "{ x: NestedArray<NestedArray<number | null> | null> }[]");
    }

    #[test]
    fn a_statement_the_database_refused_is_unknown() {
        let mut a = analysis(json!([]), json!([]), "many");
        a["diagnostics"] = json!([{ "message": "no", "start": 0, "length": 1, "code": 6 }]);
        assert_eq!(printed(&a, "sqlite", &Value::Null, false).text(), "type Params = unknown;\ntype Returns = unknown;\n");
        assert_eq!(Printed::unknown().text(), "type Params = unknown;\ntype Returns = unknown;\n");
    }

    #[test]
    fn a_name_takes_quotes_where_it_is_no_identifier() {
        assert_eq!(ident("a b"), "\"a b\"");
        assert_eq!(ident("$ok_1"), "$ok_1");
        assert_eq!(bare_name(&json!({ "label": "app.\"Odd\"\"\"" })), "Odd\"");
        assert_eq!(bare_name(&json!({ "label": "public.vector" })), "vector");
    }

    #[test]
    fn a_json_schema_reads_as_its_type() {
        let schema = json!({ "type": "object", "properties": { "a": { "type": "string" }, "b": { "type": ["integer", "null"] } }, "required": ["a"], "additionalProperties": false });
        assert_eq!(schema_type(&schema, false), "{ a: string; b?: number | null }");
        assert_eq!(schema_type(&json!({ "enum": ["x", 1, 1.5, true, null] }), false), r#""x" | 1 | 1.5 | true | null"#);
        assert_eq!(schema_type(&json!({ "type": "object" }), false), "Record<string, Json>");
        // An array index comes first, as JavaScript orders an object's keys.
        let keyed = json!({ "type": "object", "properties": { "name": { "type": "string" }, "10": { "type": "null" }, "2": { "type": "boolean" }, "01": { "type": "null" } }, "required": ["name"], "additionalProperties": false });
        assert_eq!(schema_type(&keyed, false), r#"{ "2"?: boolean; "10"?: null; name: string; "01"?: null }"#);
        assert_eq!(literal_type(&json!({ "b": 1, "1": 2 })), r#"{ "1": 2; b: 1 }"#);
    }
}
