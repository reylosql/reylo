//! JSON Schema as a rendering of the type model: what a value of a [`Type`] looks like once it is
//! JSON. A spelling's format tells a client which decoder a value needs, so each one is stated
//! even where JSON has a native type.
//!
//! One type reads two ways, by where the value stands. A value the caller decodes itself (a
//! result column, a parameter) takes the caller's representation: `int8` as a digit string
//! unless [`SchemaOptions::int8_as_number`], `numeric` as a string, `bytea` as base64. A value
//! the database already serialized into a JSON document ([`SchemaOptions::in_json`], and every
//! field of an `Object` or item of a `List`) takes the database's: `int8` and `numeric` as JSON
//! numbers, `bytea` as its own `\x…` hex.

use crate::types::{Bounds, Field, IntegerType, JsonType, Kind, Slot, StructType, Type, UuidType};
use serde_json::{json, Map, Value};

/// Where a value stands and how the caller encodes what it decodes itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchemaOptions {
    /// `int8` as a JSON number rather than a digit string where the caller decodes it (a
    /// host's `encoding.int8: number`). Inside a JSON document it is a number either way.
    pub int8_as_number: bool,
    /// The value is inside a JSON document the database wrote.
    pub in_json: bool,
    /// The value is one the caller passes (a parameter), not one the database returns: an
    /// array's elements are then non-null unless the statement says otherwise, where a result's
    /// may be null unless the statement shows they are not.
    pub input: bool,
}

impl SchemaOptions {
    /// A value inside a JSON document, where `int8_as_number` has nothing to decide.
    pub const IN_JSON: SchemaOptions = SchemaOptions { int8_as_number: false, in_json: true, input: false };

    /// A value the caller decodes itself: a result column, a parameter.
    pub fn wire(int8_as_number: bool) -> SchemaOptions {
        SchemaOptions { int8_as_number, in_json: false, input: false }
    }

    /// The same options for a value the caller passes.
    pub fn as_input(self) -> SchemaOptions {
        SchemaOptions { input: true, ..self }
    }

    /// The same options for a value inside a JSON document.
    pub fn inside_json(self) -> SchemaOptions {
        SchemaOptions { in_json: true, ..self }
    }
}

/// The schema admitting null as well: a bare `type` becomes the two-member list, unless it is
/// `null` already, and anything else an `anyOf` with null; the empty schema already admits
/// everything.
pub fn nullable(schema: Value) -> Value {
    match schema {
        Value::Object(map) if map.len() == 1 && map.contains_key("type") => match &map["type"] {
            Value::String(t) if t == "null" => Value::Object(map),
            Value::String(t) => json!({"type": [t, "null"]}),
            _ => json!({"anyOf": [Value::Object(map), {"type": "null"}]}),
        },
        Value::Object(map) if map.is_empty() => Value::Object(map),
        other => json!({"anyOf": [other, {"type": "null"}]}),
    }
}

/// A structured parameter's member: an array there keeps NULL elements, which the run time binds,
/// so an array a statement built with NULL elements goes back into a row; a top-level array
/// parameter's elements stay strict.
fn member_schema(ty: &Type, opts: SchemaOptions) -> Value {
    match &ty.kind {
        Kind::Array(a) if opts.input => json!({"type": "array", "items": nullable(member_schema(&a.element.ty, opts))}),
        _ => ty.json_schema(opts),
    }
}

/// A built-in type's name, which is its label: a type of another schema has its schema in its label
/// (`public.point`), so that a composite of the user's named so is no geometric type.
fn builtin_name(ty: &Type) -> &str {
    ty.label.as_deref().unwrap_or("")
}

/// The type's name without the schema that qualifies it: `hstore` of `public.hstore`.
fn bare_name(ty: &Type) -> &str {
    ty.label.as_deref().map(|n| n.rsplit('.').next().unwrap_or(n)).unwrap_or("")
}

/// The Postgres types whose form inside a JSON document their kind does not say, and which Postgres
/// writes there as their text, having no cast to JSON: types the format structures (a bit string, a
/// vector, a geometric type, a network address) and built-in system types, and the extensions'
/// on Neon and Supabase (PostGIS's others, cube, seg, isn, intarray, h3, hll, ip4r, pgx_ulid,
/// prefix, rdkit, roaringbitmap, semver, unit, VectorChord). An extension's type named nowhere
/// here may have a cast to JSON, so inside a document it is any JSON value. The same list in
/// src/schema.ts, `POSTGRES_IN_JSON`.
pub const POSTGRES_TEXT_IN_JSON: &[&str] = &[
    "bit", "varbit", "vector", "halfvec", "sparsevec", "point", "line", "lseg", "box", "path", "polygon", "circle", "inet", "cidr", "money", "tsvector", "tsquery", "regclass", "regtype", "ltree", "lquery", "ltxtquery",
    "oid", "pg_lsn", "jsonpath", "xid", "xid8", "oid8", "regdatabase", "regproc", "regprocedure", "regoper", "regoperator", "regnamespace", "regrole", "regcollation", "regconfig", "regdictionary",
    "geography", "box2d", "box3d", "spheroid", "raster", "cube", "seg", "ean13", "isbn", "isbn13", "ismn", "ismn13", "issn", "issn13", "upc", "query_int", "h3index", "hll", "hll_hashval",
    "ip4", "ip4r", "ip6", "ip6r", "ipaddress", "iprange", "ulid", "prefix_range", "mol", "qmol", "xqmol", "reaction", "bfp", "sfp", "roaringbitmap", "s3vec",
    "semver", "semverrange", "semvermultirange", "unit", "unitrange", "unitmultirange", "rabitq8", "rabitq4",
];

/// An integer's format: OpenAPI's `int32` or `int64` where its range fits one, an unsigned type
/// needing a bit more, and none past 64 bits, as an unsigned 64-bit integer reaches.
fn integer_format(i: &IntegerType) -> Option<&'static str> {
    match i.bits + u32::from(i.unsigned) {
        ..=32 => Some("int32"),
        ..=64 => Some("int64"),
        _ => None,
    }
}

/// A schema no value meets: a union of no members, which JSON Schema 2020-12 has no empty `anyOf` for.
fn nothing() -> Value {
    json!({"not": {}})
}

/// A value the database writes as its text, where its own JSON form is a string too.
fn text_of(ty: &Type) -> Value {
    json!({"type": "string", "description": format!("pg type {}", ty.label.as_deref().unwrap_or(ty.kind.name()))})
}

/// An I-Regexp (RFC 9485), which matches a whole value, as the ECMAScript expression JSON Schema's
/// `pattern` reads: anchored at both ends, each `.` outside a class the `[^\n\r]` I-Regexp means by
/// it (RFC 9485 §5.3), and a `^` or a `$`, which I-Regexp reads as itself, escaped.
pub fn ecma_pattern(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 8);
    let (mut escaped, mut class) = (false, false);
    for c in source.chars() {
        match c {
            _ if escaped => {
                out.push(c);
                escaped = false;
            }
            '\\' => {
                out.push(c);
                escaped = true;
            }
            '[' if !class => {
                out.push(c);
                class = true;
            }
            ']' if class => {
                out.push(c);
                class = false;
            }
            '.' if !class => out.push_str("[^\\n\\r]"),
            // Plain characters in I-Regexp, which has no anchors, and anchors in ECMAScript.
            '^' | '$' if !class => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    format!("^(?:{out})$")
}

/// A slot's schema: its type's, admitting null where the slot may hold it.
fn slot_schema(s: &Slot, opts: SchemaOptions) -> Value {
    let schema = s.ty.json_schema(opts);
    if s.nullable { nullable(schema) } else { schema }
}

// The fields of a date's and an instant's text, each within its range: a year from 0001 to 9999, a
// month and a day, a time of day, and an offset from UTC within RFC 3339's 23:59 where the value
// stands on its own. Macros, so that `concat!` joins them into the patterns below; the same in
// src/schema.ts.
macro_rules! year { () => { "(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3})" }; }
macro_rules! month_day { () => { "-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])" }; }
macro_rules! clock { () => { "T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]+)?" }; }
macro_rules! offset { () => { "[+-]([01][0-9]|2[0-3]):[0-5][0-9]" }; }
// ISO 8601's extended year of a value outside 0000–9999, as the run time writes it: a sign and six
// digits or more, never `-000000`.
macro_rules! extended_year { () => { concat!("([+-](0[1-9][0-9]{4}|[1-9][0-9]{5,})|-00", year!(), ")") }; }
// An instant's time as Postgres writes it inside a document, in the session's zone: its offset to the
// second before the zone's first standard time, and as far as 169 hours under a POSIX zone
// (`XXX-167:59:60YYY` in its summer), whose offset Postgres reads up to 168 hours and summer time
// moves one more; the hours in two digits below 100, as Postgres pads them.
macro_rules! document_time { () => { concat!(clock!(), "[+-](([0-9]{2}|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00)") }; }
// Of those offsets, the ones RFC 3339's refuses: to the second, or 24 hours or more. A four-digit
// year with any other is the format's, which checks the calendar too.
macro_rules! odd_offset { () => { "[+-](([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]|(2[4-9]|[3-9][0-9]|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00)" }; }

/// A year that RFC 3339's four digits do not write, as a date's (`DATE_PAST`) or an instant's
/// (`INSTANT_PAST`) text has it: ISO 8601's extended year where the caller decodes the value, and
/// inside a document Postgres's own, five digits or more, or four with BC after the value
/// (`12345-01-01`, `0002-06-01 BC`). Inside a document an instant of a year from 1 to 9999 takes
/// Postgres's offset too where the format refuses it (`1849-12-31T19:03:58-04:56:02`,
/// `odd_offset`). The same in src/schema.ts.
const DATE_PAST: [&str; 2] = [concat!("^", extended_year!(), month_day!(), "$"), concat!("^([1-9][0-9]{4,}", month_day!(), "|", year!(), month_day!(), " BC)$")];
const INSTANT_PAST: [&str; 2] = [
    concat!("^", extended_year!(), month_day!(), clock!(), "(Z|", offset!(), ")$"),
    concat!("^([1-9][0-9]{4,}", month_day!(), document_time!(), "|", year!(), month_day!(), document_time!(), " BC|", year!(), month_day!(), clock!(), odd_offset!(), ")$"),
];

/// The text of a year outside 0000–9999, which a bound with a sign says the type reaches, of
/// `past`'s two: where the caller decodes the value, or inside a document.
fn past_years(bounds: &Bounds, past: [&'static str; 2], opts: SchemaOptions) -> Option<&'static str> {
    let signed = [&bounds.min, &bounds.max, &bounds.exclusive_min, &bounds.exclusive_max].into_iter().flatten().any(|b| b.starts_with(['+', '-']));
    signed.then_some(past[usize::from(opts.in_json)])
}

/// A date's or a timestamp's format and, beside it, the text of each value it refuses: a year
/// outside 0000–9999 (`past_years`), and infinity, which Postgres writes as the text `infinity` or
/// `-infinity`.
fn beside_format(format: &str, past: Option<&str>, infinity: bool) -> Value {
    let mut forms = vec![json!({"type": "string", "format": format})];
    forms.extend(past.map(|pattern| json!({"type": "string", "pattern": pattern})));
    if infinity {
        forms.push(json!({"enum": ["infinity", "-infinity"]}));
    }
    if forms.len() == 1 { forms.remove(0) } else { json!({"anyOf": forms}) }
}

/// The values JSON writes as text when a float or a decimal holds them.
fn non_finite(infinity: bool, nan: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if nan {
        out.push("NaN");
    }
    if infinity {
        out.extend(["Infinity", "-Infinity"]);
    }
    out
}

impl Type {
    /// The JSON Schema of a value of this type, standing where `opts` says.
    pub fn json_schema(&self, opts: SchemaOptions) -> Value {
        match &self.kind {
            Kind::Boolean(_) => json!({"type": "boolean"}),
            // An enum's values as its base writes them: a boolean or a number as itself where its
            // schema is a JSON number or boolean, else as its text.
            Kind::Enum(e) => {
                let mut base = e.ty.json_schema(opts);
                let as_json = |v: &str| -> Value {
                    match base.get("type").and_then(Value::as_str) {
                        Some("boolean") => Value::Bool(v == "true"),
                        Some("integer" | "number") => serde_json::from_str::<serde_json::Number>(v).map(Value::Number).unwrap_or_else(|_| Value::String(v.into())),
                        _ => Value::String(v.into()),
                    }
                };
                let values: Vec<Value> = e.texts().map(as_json).collect();
                if let Value::Object(map) = &mut base {
                    map.insert("enum".into(), Value::Array(values));
                    base
                } else {
                    json!({"enum": values})
                }
            }
            // An oid is not one of the numbers JSON has a number for: Postgres writes its text.
            Kind::Integer(_) if bare_name(self) == "oid" => text_of(self),
            // A wide integer is its digits where the caller decodes it, unless `int8_as_number`.
            Kind::Integer(i) => {
                let format = integer_format(i);
                let digits = format != Some("int32") && !opts.in_json && !opts.int8_as_number;
                let mut schema = json!({"type": if digits { "string" } else { "integer" }});
                if let Some(format) = format {
                    schema["format"] = json!(format);
                }
                schema
            }
            Kind::Float(f) => {
                let number = json!({"type": "number", "format": if f.exponent_bits <= 8 { "float" } else { "double" }});
                // JSON has no number for a non-finite value, so Postgres writes one inside a
                // document as text.
                let text = non_finite(f.infinity, f.nan);
                if opts.in_json && !text.is_empty() { json!({"anyOf": [number, {"enum": text}]}) } else { number }
            }
            Kind::Decimal(d) if opts.in_json => {
                let number = json!({"type": "number", "format": "decimal"});
                let text = non_finite(d.infinity, d.nan);
                if text.is_empty() { number } else { json!({"anyOf": [number, {"enum": text}]}) }
            }
            Kind::Decimal(_) => json!({"type": "string", "format": "decimal"}),
            Kind::Text(t) => match &t.pattern {
                Some(pattern) => json!({"type": "string", "pattern": ecma_pattern(pattern)}),
                None => json!({"type": "string"}),
            },
            Kind::Uuid(UuidType { versions: None }) => json!({"type": "string", "format": "uuid"}),
            // RFC 9562's text form, its version digit one of its versions and its variant fixed.
            Kind::Uuid(UuidType { versions: Some(versions) }) => {
                let digits: String = versions.iter().map(|v| format!("{v:x}")).collect();
                let digit = if digits.len() == 1 { digits } else { format!("[{digits}]") };
                json!({"type": "string", "format": "uuid", "pattern": format!("^[0-9a-fA-F]{{8}}-[0-9a-fA-F]{{4}}-{digit}[0-9a-fA-F]{{3}}-[89abAB][0-9a-fA-F]{{3}}-[0-9a-fA-F]{{12}}$")})
            }
            // JSON Schema's `date-time` and `time` are RFC 3339's, which need a UTC offset: a value
            // that keeps one takes the format, and a wall-clock value, which is no instant, does not.
            // A time that reaches 24:00:00, as Postgres's do, is past RFC 3339's too.
            Kind::Instant(i) => beside_format("date-time", past_years(&i.bounds, INSTANT_PAST, opts), i.infinity),
            Kind::ZonedDateTime(z) if z.offset_only => beside_format("date-time", None, z.infinity),
            Kind::ZonedDateTime(_) => json!({"type": "string", "description": "a date and time with its time zone, a region or an offset"}),
            Kind::DateTime(_) => json!({"type": "string", "description": "a date and time without a time zone, as the database wrote it (2026-10-20T10:00:00.123456)"}),
            Kind::Date(d) => beside_format("date", past_years(&d.bounds, DATE_PAST, opts), d.infinity),
            Kind::Time(t) if t.offset && t.bounds.max.as_deref() != Some("24:00:00") => json!({"type": "string", "format": "time"}),
            Kind::Time(t) if t.offset => text_of(self),
            Kind::Time(_) => json!({"type": "string", "description": "a time of day without a time zone (10:00:00.5)"}),
            Kind::Bytes(_) if matches!(bare_name(self), "macaddr" | "macaddr8") => text_of(self),
            // Postgres writes its own hex inside a document.
            Kind::Bytes(_) if opts.in_json => json!({"type": "string", "format": "hex"}),
            // `format: byte` is the table's word; `contentEncoding` is the JSON Schema 2020-12
            // spelling of the same base64, kept beside it for validators.
            Kind::Bytes(_) => json!({"type": "string", "format": "byte", "contentEncoding": "base64"}),
            // A bit string and pgvector's vectors are their text to a client.
            Kind::Array(_) if matches!(builtin_name(self), "bit" | "varbit") || matches!(bare_name(self), "vector" | "halfvec" | "sparsevec") => text_of(self),
            // Postgres has no cast of a geometric type to JSON, so a document holds its text.
            // A composite of the user's named so, `public.point`, is an object of its fields.
            Kind::Array(_) | Kind::Struct(_) if opts.in_json && matches!(builtin_name(self), "point" | "line" | "lseg" | "box" | "path" | "polygon" | "circle") => text_of(self),
            Kind::Array(a) => {
                let items = a.element.ty.json_schema(opts);
                // A parameter's elements are strict; a result's may be NULL unless the statement
                // shows they are not.
                let nulls = a.element.nullable && !opts.input;
                json!({"type": "array", "items": if nulls { nullable(items) } else { items }})
            }
            // A row's fields stand where the row does. A field that may be null is spelled as an
            // `anyOf` with null in a record with a name (a composite, an anonymous `record`), and is
            // widened as `nullable` widens any value in one without (an object a statement built).
            Kind::Struct(r) => {
                let mut schema = object(&r.fields, |f| {
                    let schema = f.ty.json_schema(opts);
                    match (f.nullable, self.label.is_some()) {
                        (false, _) => schema,
                        (true, true) => json!({"anyOf": [schema, {"type": "null"}]}),
                        (true, false) => nullable(schema),
                    }
                });
                if let (Some(extra), Value::Object(map)) = (&r.extra, &mut schema) {
                    map.insert("additionalProperties".into(), slot_schema(extra, opts));
                }
                schema
            }
            // A table nested in a value as an array of objects, a row each.
            Kind::Table(t) => json!({"type": "array", "items": object(&t.columns, |c| slot_schema(&c.slot(), opts))}),
            // What a statement built: its keys and items are inside the document.
            Kind::Json(JsonType { content: Some(content), .. }) => {
                let schema = content.ty.json_schema(opts.inside_json());
                if content.nullable { nullable(schema) } else { schema }
            }
            // The values a CHECK's JSON Schema admits, as the engine reduced it; else any JSON,
            // null included.
            Kind::Json(JsonType { schema: Some(_), .. }) => self.schema().unwrap_or(Value::Bool(true)),
            Kind::Json(_) | Kind::Unknown => json!({}),
            Kind::Null => json!({"type": "null"}),
            // An hstore is an object of its keys inside a document, each value text or null.
            Kind::Map(_) if bare_name(self) == "hstore" && opts.in_json => json!({"type": "object", "additionalProperties": {"type": ["string", "null"]}}),
            Kind::Map(_) if bare_name(self) == "hstore" => text_of(self),
            // A map as its entries, each a key and its value, in order.
            Kind::Map(m) => {
                let entry = json!({"type": "object", "properties": {"key": slot_schema(&m.key, opts), "value": slot_schema(&m.value, opts)}, "required": ["key", "value"]});
                json!({"type": "array", "items": entry})
            }
            // A tuple as a list of its values, each of its own type: its elements, then its
            // optional ones, then any number of its rest.
            Kind::Tuple(t) => {
                let items: Vec<Value> = t.elements.iter().chain(&t.optional_elements).map(|e| slot_schema(e, opts)).collect();
                let rest = t.rest.as_ref().map_or(Value::Bool(false), |r| slot_schema(r, opts));
                // JSON Schema 2020-12 refuses an empty `prefixItems`, so a tuple of none is `items` alone.
                let mut schema = json!({"type": "array", "items": rest, "minItems": t.elements.len()});
                if !items.is_empty() {
                    schema["prefixItems"] = Value::Array(items);
                }
                if t.rest.is_none() {
                    schema["maxItems"] = json!(t.elements.len() + t.optional_elements.len());
                }
                schema
            }
            // A tagged union as a client returns one: its tag and its value, `{ tag, value }`.
            Kind::TaggedUnion(t) if t.alternatives.is_empty() => nothing(),
            Kind::TaggedUnion(t) => json!({"anyOf": t.alternatives.iter().map(|a| {
                let value = a.ty.json_schema(opts);
                json!({"type": "object", "properties": {"tag": {"const": a.tag}, "value": if a.nullable { nullable(value) } else { value }}, "required": ["tag", "value"]})
            }).collect::<Vec<_>>()}),
            Kind::Union(_) if matches!(builtin_name(self), "inet" | "cidr") => text_of(self),
            // One of several types, the execution's or each value's; of none, no value, and every
            // value of an intersection of none.
            Kind::Union(u) if u.members.is_empty() => nothing(),
            Kind::Union(u) => json!({"anyOf": u.members.iter().map(|m| m.json_schema(opts)).collect::<Vec<_>>()}),
            Kind::Intersection(u) if u.members.is_empty() => json!({}),
            Kind::Intersection(u) => json!({"allOf": u.members.iter().map(|m| m.json_schema(opts)).collect::<Vec<_>>()}),
            // A row whose columns were not read (a subquery's) is an object of them inside a document.
            Kind::Opaque if bare_name(self) == "record" && opts.in_json => json!({"type": "object"}),
            // PostGIS writes a geometry inside a document through its cast to JSON, as GeoJSON.
            Kind::Geometry(_) if bare_name(self) == "geometry" && opts.in_json => {
                json!({"type": "object", "properties": {"type": {"type": "string"}}, "required": ["type"], "description": "a GeoJSON geometry"})
            }
            // An extension's type of no form the engine knows, which may have a cast to JSON: any
            // JSON value inside a document.
            Kind::Opaque | Kind::Geometry(_) if opts.in_json && !POSTGRES_TEXT_IN_JSON.contains(&bare_name(self)) => {
                json!({"description": format!("pg type {}", self.label.as_deref().unwrap_or(self.kind.name()))})
            }
            // Every other type is a plain string, which is how Postgres prints it, named in the
            // description.
            _ => text_of(self),
        }
    }

    /// The JSON Schema of what the caller passes for a structured parameter of this type (a
    /// record, or an array of them): each key's value as it passes a parameter of that type.
    pub fn rows_schema(&self, opts: SchemaOptions) -> Value {
        let row = |r: &StructType| {
            let mut properties = Map::new();
            let mut required = Vec::new();
            for f in &r.fields {
                let schema = member_schema(&f.ty, opts);
                properties.insert(f.name.clone(), if f.nullable { nullable(schema) } else { schema });
                if !f.optional {
                    required.push(json!(f.name));
                }
            }
            json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
        };
        match &self.kind {
            Kind::Struct(r) => row(r),
            Kind::Array(a) => match &a.element.ty.kind {
                Kind::Struct(r) => json!({"type": "array", "items": row(r)}),
                _ => self.json_schema(opts),
            },
            _ => self.json_schema(opts),
        }
    }
}

/// An object of `fields`, every key required, each spelled by `field`.
fn object(fields: &[Field], field: impl Fn(&Field) -> Value) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::with_capacity(fields.len());
    for f in fields {
        properties.insert(f.name.clone(), field(f));
        required.push(json!(f.name));
    }
    json!({"type": "object", "properties": properties, "required": required})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DecimalType, FloatType, Slot};

    const WIRE: SchemaOptions = SchemaOptions { int8_as_number: false, in_json: false, input: false };
    const INT8N: SchemaOptions = SchemaOptions { int8_as_number: true, in_json: false, input: false };
    const IN_JSON: SchemaOptions = SchemaOptions::IN_JSON;

    fn numeric() -> Type {
        Type::decimal(DecimalType { infinity: true, nan: true, ..DecimalType::default() }).named("numeric")
    }

    fn float8() -> Type {
        Type::new(Kind::Float(FloatType { exponent_bits: 11, mantissa_bits: 52, infinity: true, nan: true, ..FloatType::default() })).named("float8")
    }

    fn field(name: &str, ty: Type, nullable: bool) -> Field {
        Field::new(name, ty, nullable)
    }

    #[test]
    fn each_kind_follows_the_encoding_table() {
        for (ty, wire) in [
            (Type::boolean(), json!({"type": "boolean"})),
            (Type::integer(16), json!({"type": "integer", "format": "int32"})),
            (Type::integer(32), json!({"type": "integer", "format": "int32"})),
            (Type::integer(64), json!({"type": "string", "format": "int64"})),
            (Type::single(), json!({"type": "number", "format": "float"})),
            (float8(), json!({"type": "number", "format": "double"})),
            (numeric(), json!({"type": "string", "format": "decimal"})),
            (Type::text().named("varchar"), json!({"type": "string"})),
            (Type::uuid(), json!({"type": "string", "format": "uuid"})),
            (Kind::Uuid(UuidType { versions: Some(vec![7]) }).into(), json!({"type": "string", "format": "uuid", "pattern": "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$"})),
            (Kind::Uuid(UuidType { versions: Some(vec![4, 7]) }).into(), json!({"type": "string", "format": "uuid", "pattern": "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[47][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$"})),
            (Type::null(), json!({"type": "null"})),
            (Type::bytes().named("bytea"), json!({"type": "string", "format": "byte", "contentEncoding": "base64"})),
            (Type::unknown(), json!({})),
            (Type::opaque("ltree"), json!({"type": "string", "description": "pg type ltree"})),
        ] {
            assert_eq!(ty.json_schema(WIRE), wire, "{ty:?}");
        }
        let either = Type::union(vec![Type::integer(32), Type::double()]);
        assert_eq!(either.json_schema(WIRE), json!({"anyOf": [{"type": "integer", "format": "int32"}, {"type": "number", "format": "double"}]}));
    }

    /// RFC 3339's `date-time` and `time`, which JSON Schema's formats name, need an offset, so
    /// only a value that keeps one takes them.
    #[test]
    fn only_a_time_with_an_offset_takes_rfc_3339s_formats() {
        use crate::types::{Bounds, DateTimeType, DateType, InstantType, TimeType, ZonedDateTimeType, MICROSECOND};
        let tick = || MICROSECOND.to_string();
        // Postgres writes an infinite date or timestamp as the text `infinity`, which no date format admits.
        let infinite = |schema: Value| json!({"anyOf": [schema, {"enum": ["infinity", "-infinity"]}]});
        let time = |offset: bool, max: Option<&str>| Type::new(Kind::Time(TimeType { tick: tick(), offset, bounds: Bounds { max: max.map(Into::into), ..Bounds::default() } }));
        let wall = json!({"type": "string", "description": "a date and time without a time zone, as the database wrote it (2026-10-20T10:00:00.123456)"});
        for opts in [WIRE, IN_JSON, WIRE.as_input()] {
            assert_eq!(Type::new(Kind::DateTime(DateTimeType { tick: tick(), ..DateTimeType::default() })).named("timestamp").json_schema(opts), wall);
            assert_eq!(Type::new(Kind::Instant(InstantType { tick: tick(), ..InstantType::default() })).named("timestamptz").json_schema(opts), json!({"type": "string", "format": "date-time"}));
            assert_eq!(Type::new(Kind::ZonedDateTime(ZonedDateTimeType { tick: tick(), offset_only: true, ..ZonedDateTimeType::default() })).json_schema(opts), json!({"type": "string", "format": "date-time"}));
            assert_eq!(Type::new(Kind::Instant(InstantType { tick: tick(), infinity: true, ..InstantType::default() })).named("timestamptz").json_schema(opts), infinite(json!({"type": "string", "format": "date-time"})));
            assert_eq!(Type::new(Kind::Date(DateType { infinity: true, ..DateType::default() })).named("date").json_schema(opts), infinite(json!({"type": "string", "format": "date"})));
            assert_eq!(Type::new(Kind::Date(DateType::default())).named("date").json_schema(opts), json!({"type": "string", "format": "date"}));
            assert_eq!(Type::new(Kind::ZonedDateTime(ZonedDateTimeType { tick: tick(), ..ZonedDateTimeType::default() })).json_schema(opts), json!({"type": "string", "description": "a date and time with its time zone, a region or an offset"}));
            assert_eq!(time(false, Some("24:00:00")).named("time").json_schema(opts), json!({"type": "string", "description": "a time of day without a time zone (10:00:00.5)"}));
            assert_eq!(time(false, None).json_schema(opts), json!({"type": "string", "description": "a time of day without a time zone (10:00:00.5)"}));
            // Postgres's timetz reaches 24:00:00.
            assert_eq!(time(true, Some("24:00:00")).named("timetz").json_schema(opts), json!({"type": "string", "description": "pg type timetz"}));
            assert_eq!(time(true, None).json_schema(opts), json!({"type": "string", "format": "time"}));
        }
    }

    /// A year outside 0000–9999, which a bound with a sign says Postgres's date and timestamptz
    /// reach, is none of RFC 3339's four digits: its own text stands beside the format, ISO 8601's
    /// extended year where the caller decodes the value (a sign on such a year alone, never
    /// `-000000`), and Postgres's inside a document (five digits, or BC after the value), each field
    /// within its range. tests/values.test.ts holds values to the same strings. A date that stays
    /// within those years keeps the format alone.
    #[test]
    fn a_year_outside_four_digits_stands_beside_the_format() {
        use crate::types::{Bounds, DateType, InstantType, MICROSECOND};
        let date = Type::new(Kind::Date(DateType { bounds: Bounds::between("-4713-11-24", "+5874897-12-31"), infinity: true })).named("date");
        let instant = Type::new(Kind::Instant(InstantType { tick: MICROSECOND.to_string(), bounds: Bounds::between("-4713-11-24T00:00:00Z", "+294276-12-31T23:59:59.999999Z"), infinity: true })).named("timestamptz");
        let forms = |format: &str, pattern: &str| json!({"anyOf": [{"type": "string", "format": format}, {"type": "string", "pattern": pattern}, {"enum": ["infinity", "-infinity"]}]});
        for opts in [WIRE, WIRE.as_input()] {
            assert_eq!(date.json_schema(opts), forms("date", "^([+-](0[1-9][0-9]{4}|[1-9][0-9]{5,})|-00(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3}))-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$"));
            assert_eq!(instant.json_schema(opts), forms("date-time", "^([+-](0[1-9][0-9]{4}|[1-9][0-9]{5,})|-00(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3}))-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]+)?(Z|[+-]([01][0-9]|2[0-3]):[0-5][0-9])$"));
        }
        assert_eq!(date.json_schema(IN_JSON), forms("date", "^([1-9][0-9]{4,}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])|(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3})-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01]) BC)$"));
        assert_eq!(instant.json_schema(IN_JSON), forms("date-time", "^([1-9][0-9]{4,}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]+)?[+-](([0-9]{2}|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00)|(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3})-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]+)?[+-](([0-9]{2}|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00) BC|(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3})-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]+)?[+-](([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]|(2[4-9]|[3-9][0-9]|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00))$"));
        let mysql = Type::new(Kind::Date(DateType { bounds: Bounds { min: Some("0000-01-01".into()), ..Bounds::default() }, infinity: false })).named("date");
        assert_eq!(mysql.json_schema(WIRE), json!({"type": "string", "format": "date"}));
    }

    #[test]
    fn int8_as_number_changes_only_int8_where_the_caller_decodes() {
        assert_eq!(Type::integer(64).json_schema(INT8N), json!({"type": "integer", "format": "int64"}));
        assert_eq!(numeric().json_schema(INT8N), json!({"type": "string", "format": "decimal"}));
    }

    #[test]
    fn inside_json_the_database_spells_int8_numeric_and_bytea() {
        assert_eq!(Type::integer(64).json_schema(IN_JSON), json!({"type": "integer", "format": "int64"}));
        // A non-finite float or decimal is text inside a document: `{"f": "Infinity"}`.
        assert_eq!(numeric().json_schema(IN_JSON), json!({"anyOf": [{"type": "number", "format": "decimal"}, {"enum": ["NaN", "Infinity", "-Infinity"]}]}));
        assert_eq!(float8().json_schema(IN_JSON), json!({"anyOf": [{"type": "number", "format": "double"}, {"enum": ["NaN", "Infinity", "-Infinity"]}]}));
        // A decimal that holds no infinity, `numeric(10,2)`, writes only NaN as text.
        let sized = Type::decimal(DecimalType { precision: Some(10), min_scale: Some(2), max_scale: Some(2), nan: true, ..DecimalType::default() });
        assert_eq!(sized.json_schema(IN_JSON), json!({"anyOf": [{"type": "number", "format": "decimal"}, {"enum": ["NaN"]}]}));
        assert_eq!(Type::bytes().json_schema(IN_JSON), json!({"type": "string", "format": "hex"}));
        assert_eq!(Type::opaque("oid").json_schema(IN_JSON)["type"], json!("string"));
        assert_eq!(Type::unsigned(32).named("oid").json_schema(IN_JSON)["type"], json!("string"));
    }

    #[test]
    fn inside_json_an_hstore_is_an_object_of_nullable_text() {
        let hstore = Type::new(Kind::Map(crate::types::MapType { key: Box::new(Slot::non_null(Type::text())), value: Box::new(Slot::nullable(Type::text())), size: None })).named("public.hstore");
        assert_eq!(hstore.json_schema(IN_JSON), json!({"type": "object", "additionalProperties": {"type": ["string", "null"]}}));
        assert_eq!(hstore.json_schema(WIRE), json!({"type": "string", "description": "pg type public.hstore"}));
    }

    #[test]
    fn arrays_json_and_unknown() {
        let mood = Type::text_of(vec!["ok".into(), "sad".into()]).named("public.mood");
        assert_eq!(mood.json_schema(WIRE), json!({"type": "string", "enum": ["ok", "sad"]}));
        // A literal of a number or a truth value as JSON writes it, an int8's as its text where the caller decodes it.
        assert_eq!(Type::one_of(Type::integer(32), vec!["1".into()]).json_schema(WIRE), json!({"type": "integer", "format": "int32", "enum": [1]}));
        assert_eq!(Type::one_of(Type::integer(64), vec!["1".into()]).json_schema(WIRE), json!({"type": "string", "format": "int64", "enum": ["1"]}));
        assert_eq!(Type::one_of(Type::boolean(), vec!["true".into()]).json_schema(WIRE), json!({"type": "boolean", "enum": [true]}));
        // A result's elements may be NULL, as any Postgres array's may, unless the statement shows
        // they are not; a parameter's are strict.
        let ints = Type::array(Slot::nullable(Type::integer(32)));
        assert_eq!(ints.json_schema(WIRE), json!({"type": "array", "items": {"anyOf": [{"type": "integer", "format": "int32"}, {"type": "null"}]}}));
        assert_eq!(ints.json_schema(WIRE.as_input()), json!({"type": "array", "items": {"type": "integer", "format": "int32"}}));
        let strict = Type::array(Slot::non_null(Type::integer(64)));
        assert_eq!(strict.json_schema(IN_JSON)["items"], json!({"type": "integer", "format": "int64"}));
        assert_eq!(strict.json_schema(WIRE)["items"], json!({"type": "string", "format": "int64"}));
        // A structured parameter's member keeps its array's NULL elements, which the run time binds.
        let texts = Type::array(Slot::nullable(Type::text()));
        let member = Type::structure(vec![field("tags", texts, false)]);
        assert_eq!(member.rows_schema(WIRE.as_input())["properties"]["tags"], json!({"type": "array", "items": {"type": ["string", "null"]}}));
        assert_eq!(Type::json().json_schema(WIRE), json!({}));
        let checked = json!({"type": "object", "properties": {"a": {"type": "string"}}, "additionalProperties": true});
        assert_eq!(Type::json_schema_of(checked.clone()).json_schema(WIRE), checked);
        assert_eq!(Type::unknown().json_schema(IN_JSON), json!({}));
    }

    #[test]
    fn a_rows_fields_stand_where_the_row_does() {
        let users = Type::structure(vec![field("id", Type::integer(64), false), field("name", Type::text(), true), field("meta", Type::json(), true)]).named("public.users");
        assert_eq!(
            users.json_schema(WIRE),
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "format": "int64"},
                    "name": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                    "meta": {"anyOf": [{}, {"type": "null"}]},
                },
                "required": ["id", "name", "meta"],
            })
        );
        assert_eq!(users.json_schema(IN_JSON)["properties"]["id"], json!({"type": "integer", "format": "int64"}));
        assert_eq!(users.json_schema(INT8N)["properties"]["id"], json!({"type": "integer", "format": "int64"}));
        let many = Type::array(Slot::nullable(users.clone()));
        assert_eq!(many.json_schema(WIRE), json!({"type": "array", "items": nullable(users.json_schema(WIRE))}));
        assert_eq!(many.json_schema(WIRE.as_input()), json!({"type": "array", "items": users.json_schema(WIRE)}));
    }

    #[test]
    fn a_constructors_keys_and_items_are_inside_the_document() {
        let post = Type::structure(vec![field("id", Type::integer(64), false), field("title", Type::text(), true), field("body", Type::unknown(), true), field("blob", Type::bytes(), false)]);
        let expected = json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer", "format": "int64"},
                "title": {"type": ["string", "null"]},
                "body": {},
                "blob": {"type": "string", "format": "hex"},
            },
            "required": ["id", "title", "body", "blob"],
        });
        // The same whatever `int8_as_number` says.
        let built = Type::json_of(Slot::non_null(post.clone()));
        assert_eq!(built.json_schema(WIRE), expected);
        assert_eq!(built.json_schema(INT8N), expected);
        let list = Type::json_of(Slot::non_null(Type::array(Slot::nullable(numeric()))));
        assert_eq!(list.json_schema(WIRE), json!({"type": "array", "items": {"anyOf": [{"anyOf": [{"type": "number", "format": "decimal"}, {"enum": ["NaN", "Infinity", "-Infinity"]}]}, {"type": "null"}]}}));
        let posts = Type::json_of(Slot::non_null(Type::array(Slot::non_null(post))));
        assert_eq!(posts.json_schema(WIRE), json!({"type": "array", "items": expected}));
        // A key that is always null admits null alone, which `type` names once.
        let none = Type::json_of(Slot::non_null(Type::structure(vec![field("a", Type::null(), true)])));
        assert_eq!(none.json_schema(WIRE)["properties"]["a"], json!({"type": "null"}));
    }

    #[test]
    fn a_tuple_struct_extra_and_pattern_read_as_json_schema_writes_them() {
        let tuple = Type::new(Kind::Tuple(crate::types::TupleType { elements: vec![Slot::non_null(Type::text())], optional_elements: vec![Slot::nullable(Type::integer(32))], rest: None }));
        assert_eq!(tuple.json_schema(WIRE), json!({"type": "array", "prefixItems": [{"type": "string"}, {"anyOf": [{"type": "integer", "format": "int32"}, {"type": "null"}]}], "items": false, "minItems": 1, "maxItems": 2}));
        let open = Type::new(Kind::Struct(StructType { fields: vec![field("a", Type::integer(32), false)], extra: Some(Box::new(Slot::non_null(Type::text()))) }));
        assert_eq!(open.json_schema(WIRE)["additionalProperties"], json!({"type": "string"}));
        assert_eq!(ecma_pattern("a.b[.]\\."), "^(?:a[^\\n\\r]b[.]\\.)$");
        assert_eq!(ecma_pattern("$5[^a]"), "^(?:\\$5[^a])$");
    }

    /// OpenAPI's `int64` holds a signed 64-bit integer: an unsigned one of 64 bits, which MySQL's
    /// BIGINT UNSIGNED and ClickHouse's UInt64 reach, takes no format, nor does a wider one.
    #[test]
    fn an_integer_past_a_signed_64_bits_takes_no_format() {
        let unsigned = |bits: u32| Type::new(Kind::Integer(IntegerType { bits, unsigned: true, ..IntegerType::default() }));
        assert_eq!(unsigned(32).json_schema(IN_JSON), json!({"type": "integer", "format": "int64"}));
        assert_eq!(unsigned(64).json_schema(IN_JSON), json!({"type": "integer"}));
        assert_eq!(unsigned(64).json_schema(WIRE), json!({"type": "string"}));
        assert_eq!(unsigned(64).json_schema(INT8N), json!({"type": "integer"}));
        assert_eq!(Type::integer(128).json_schema(IN_JSON), json!({"type": "integer"}));
        assert_eq!(Type::integer(64).json_schema(IN_JSON), json!({"type": "integer", "format": "int64"}));
    }

    /// JSON Schema 2020-12 refuses an empty `prefixItems`, `anyOf` and `allOf`.
    #[test]
    fn a_container_of_nothing_is_a_schema_still() {
        use crate::types::{TaggedUnionType, TupleType, UnionType};
        let tuple = Type::new(Kind::Tuple(TupleType { elements: vec![], optional_elements: vec![], rest: None }));
        assert_eq!(tuple.json_schema(IN_JSON), json!({"type": "array", "items": false, "minItems": 0, "maxItems": 0}));
        assert_eq!(Type::new(Kind::Union(UnionType { members: vec![] })).json_schema(WIRE), json!({"not": {}}));
        assert_eq!(Type::new(Kind::Intersection(UnionType { members: vec![] })).json_schema(WIRE), json!({}));
        assert_eq!(Type::new(Kind::TaggedUnion(TaggedUnionType { alternatives: vec![] })).json_schema(WIRE), json!({"not": {}}));
    }

    /// Inside a document Postgres writes a type with no cast to JSON as its text; an extension's
    /// type named nowhere may have a cast, so it is any JSON value there, and its text elsewhere.
    #[test]
    fn inside_json_a_type_of_no_known_form_is_any_json() {
        assert_eq!(Type::opaque("ltree").json_schema(IN_JSON), json!({"type": "string", "description": "pg type ltree"}));
        assert_eq!(Type::opaque("money").json_schema(IN_JSON), json!({"type": "string", "description": "pg type money"}));
        assert_eq!(Type::opaque("public.unheard_of").json_schema(IN_JSON), json!({"description": "pg type public.unheard_of"}));
        assert_eq!(Type::opaque("public.unheard_of").json_schema(WIRE), json!({"type": "string", "description": "pg type public.unheard_of"}));
        // A built-in structure is known by its own label, which a composite of the user's, whose label is qualified, never has.
        let point = |name: &str| Type::structure(vec![field("x", Type::double(), false)]).named(name);
        assert_eq!(point("point").json_schema(IN_JSON), json!({"type": "string", "description": "pg type point"}));
        assert_eq!(point("public.point").json_schema(IN_JSON)["type"], json!("object"));
    }

    #[test]
    fn nullable_widens_a_bare_type_and_wraps_the_rest() {
        assert_eq!(nullable(json!({"type": "string"})), json!({"type": ["string", "null"]}));
        assert_eq!(nullable(json!({"type": "string", "format": "uuid"})), json!({"anyOf": [{"type": "string", "format": "uuid"}, {"type": "null"}]}));
        assert_eq!(nullable(json!({"type": ["string", "null"]})), json!({"anyOf": [{"type": ["string", "null"]}, {"type": "null"}]}));
        assert_eq!(nullable(json!({})), json!({}));
        assert_eq!(nullable(json!({"$ref": "#/components/schemas/X"})), json!({"anyOf": [{"$ref": "#/components/schemas/X"}, {"type": "null"}]}));
    }
}
