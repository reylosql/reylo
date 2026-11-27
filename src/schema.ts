// The JSON Schema of a query's input and output: `query.inputSchema`, `query.outputSchema`, and
// Standard Schema's `jsonSchema` converter. A build step writes both into the module. The
// spellings are the engine's (engine/core/src/schema.rs); what differs
// is where a value stands. The output is the result as JSON writes it (`AsJson<T>` in src/index.ts)
// of the value `run` returns, which is each type's one representation (src/values.ts, the same
// table the run time decodes with): a 64-bit integer, a BigInt, as the JSON integer it is (what a
// serializer that writes a BigInt exactly writes, `JSON.rawJSON(String(v))`; JSON.stringify itself
// refuses a BigInt), a decimal its text, a timestamptz its ISO text in UTC, a date and a timestamp
// the wall-clock text the server wrote. The input is the parameters the caller passes.
import { bareName, isMysqlSet, nestsAnyDeep, schemaOf, type EngineType, type JsonSchema } from "./engine-type.ts";
import { columnKind, RANGE_BOUNDS, sqliteAffinity, sqliteWide, TS_TYPE, type ScalarKind } from "./values.ts";
import { leafCodec, type Codec, type Codecs } from "./codecs.ts";

type Dialect = "postgres" | "mysql" | "sqlite" | "clickhouse";
type Schema = { [key: string]: unknown };
type Analysis = {
  /** The prose written in comments before the statement, which describes both schemas. */
  comment?: string;
  params: readonly { name: string; type: EngineType; nullable?: boolean; optional?: boolean; comment?: string; schema?: JsonSchema; members?: readonly { written: string; schema?: JsonSchema }[] }[];
  columns: readonly { name: string; type: EngineType; nullable?: boolean; comment?: string }[];
  cardinality: "one" | "at_most_one" | "many" | "none";
  value?: boolean;
};

/** The schema admitting null as well: a bare `type` gets `"null"` beside it, unless it is `"null"` already, and anything else an `anyOf` with null. */
function nullable(schema: Schema | JsonSchema): Schema | JsonSchema {
  if (schema === true || (typeof schema === "object" && Object.keys(schema).length === 0)) return schema;
  if (typeof schema === "object" && Object.keys(schema).length === 1 && typeof schema.type === "string") return schema.type === "null" ? schema : { type: [schema.type, "null"] };
  return { anyOf: [schema, { type: "null" }] };
}

/** A PostGIS geometry inside a document, which its cast to JSON writes as GeoJSON. */
const GEOJSON: Schema = { type: "object", properties: { type: { type: "string" } }, required: ["type"], description: "a GeoJSON geometry" };

/** A schema no value meets: a union of no members. JSON Schema 2020-12 refuses an empty `anyOf`, `allOf` or `prefixItems`. */
const NOTHING: Schema = { not: {} };

/** Any JSON value, as a module names it: the type `reylo` exports. */
export const JSON_TYPE = "Json";

/** A value's form inside a JSON document a database built: its JSON Schema, and its TypeScript type. */
export type DocumentForm = { schema: Schema; type: string };

const ANY: DocumentForm = { schema: {}, type: JSON_TYPE };
const BOOLEAN: DocumentForm = { schema: { type: "boolean" }, type: "boolean" };
const string = (schema: Schema = { type: "string" }): DocumentForm => ({ schema, type: "string" });

// What Postgres writes inside a JSON document for a type whose form there its kind does not say
// (the spec's facts): the text of a type the format structures (a bit string, a vector, a
// geometric type, a network address) or does not (an extension's), an object of its keys for an
// hstore, GeoJSON for a geometry, and an object of its columns for a row whose columns were not
// read. A type named nowhere here, whose form the engine does not know, is any JSON value: an
// extension's type may have a cast to JSON. The same list in engine/core/src/schema.rs and
// engine/cli/src/typescript.rs.
export const POSTGRES_IN_JSON: Readonly<Record<string, DocumentForm>> = {
  ...Object.fromEntries(
    [
      "bit", "varbit", "vector", "halfvec", "sparsevec",
      "point", "line", "lseg", "box", "path", "polygon", "circle",
      "inet", "cidr",
      "money", "tsvector", "tsquery", "regclass", "regtype",
      "ltree", "lquery", "ltxtquery",
      // Types with no cast to JSON, which Postgres writes inside a document as their text: built-in
      // system types, and the extensions' on Neon and Supabase (PostGIS's others, cube, seg, isn,
      // intarray, h3, hll, ip4r, pgx_ulid, prefix, rdkit, roaringbitmap, semver, unit, VectorChord).
      "pg_lsn", "jsonpath", "xid", "xid8", "oid8", "regdatabase",
      "regproc", "regprocedure", "regoper", "regoperator", "regnamespace", "regrole", "regcollation", "regconfig", "regdictionary",
      "geography", "box2d", "box3d", "spheroid", "raster",
      "cube", "seg", "ean13", "isbn", "isbn13", "ismn", "ismn13", "issn", "issn13", "upc",
      "query_int", "h3index", "hll", "hll_hashval",
      "ip4", "ip4r", "ip6", "ip6r", "ipaddress", "iprange", "ulid", "prefix_range",
      "mol", "qmol", "xqmol", "reaction", "bfp", "sfp", "roaringbitmap", "s3vec",
      "semver", "semverrange", "semvermultirange", "unit", "unitrange", "unitmultirange", "rabitq8", "rabitq4",
    ].map((name) => [name, string({ type: "string", description: `pg type ${name}` })]),
  ),
  // An hstore is an object of its keys inside a document, each value text or null (`{"a": "1", "b": null}`).
  hstore: { schema: { type: "object", additionalProperties: { type: ["string", "null"] } }, type: "Record<string, string | null>" },
  // PostGIS writes a geometry through its cast to JSON: a GeoJSON geometry, `{"type":"Point","coordinates":[1,2]}`.
  geometry: { schema: GEOJSON, type: "{ type: string; [key: string]: Json | undefined }" },
  // A row whose columns were not read (a subquery's): an object of them inside a document.
  record: { schema: { type: "object" }, type: "Record<string, Json>" },
};

/**
 * MySQL's bytes inside a document, a BIT's at any width among them: `base64:type<n>:` and their
 * base64, `<n>` the column's type code (15 for a VARBINARY, 16 for a BIT, 252 for a BLOB, 254 for a
 * BINARY).
 */
const MYSQL_BYTES = string({ type: "string", pattern: "^base64:type[0-9]+:([A-Za-z0-9+/]{4})*([A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$", description: "bytes as MySQL writes them inside JSON, their type code and their base64 (base64:type15:YWI=)" });

/**
 * How `dialect` writes a value of `t` inside a JSON document it builds, where the value is no
 * structure the writers walk: the one table both the output schema (`plainSchema`) and the declared
 * type (src/render.ts) read, so the two cannot disagree. Undefined for an array, a row, a tuple, an
 * enum, a union and a document, each of whose parts it answers for. `inUnion`: the value is a
 * union's member, where a float's or a decimal's NaN and infinities stay the text Postgres writes,
 * which the run time converts only where the value stands alone (src/values.ts, `numbersInJson`).
 */
export function inDocument(t: EngineType, dialect: Dialect, inUnion = false): DocumentForm | undefined {
  const name = bareName(t);
  if (dialect === "sqlite") {
    // SQLite writes a value as its storage class: a number, text, or a NUMERIC's either; a column
    // declared BOOLEAN holds 0 and 1, which it writes as numbers. A BLOB is no JSON.
    switch (t.kind) {
      case "integer": return { schema: { type: "integer", format: "int64" }, type: "number" };
      case "float": return { schema: { type: "number", format: "double" }, type: "number" };
      case "boolean": return BOOLEAN;
      case "text": return string();
      case "union": return /^bool(ean)?$/i.test(name.trim()) ? { schema: { type: "integer", format: "int64" }, type: "number" } : name.toUpperCase() === "NUMERIC" ? { schema: { type: ["number", "string"] }, type: "number | string" } : ANY;
      case "array": return isMysqlSet(t) ? ANY : undefined;
      case "enum": case "tuple": case "struct": case "json": case "table": case "intersection": case "tagged_union": case "null": case "unknown": return undefined;
      default: return ANY;
    }
  }
  if (dialect === "postgres") {
    switch (t.kind) {
      case "array": case "struct": case "union":
        // A type the format structures that Postgres writes as its text, or a network address.
        return t.kind === "struct" && name === "record" ? undefined : POSTGRES_IN_JSON[name];
      // An oid is no number JSON writes: Postgres writes its text.
      case "integer": return name === "oid" ? string() : { schema: integer(t), type: "number" };
      case "bytes": return name === "macaddr" || name === "macaddr8" ? string({ type: "string", description: `pg type ${name}` }) : string({ type: "string", format: "hex" });
      // Postgres's own interval text: the run time converts a column's interval, never one inside a document.
      case "duration": return string({ type: "string", description: "pg type interval" });
      // A range is its text inside a document, `[1,5)`.
      case "range": case "multirange": return string({ type: "string", description: `pg type ${name || t.kind}` });
      case "xml": return string({ type: "string", description: "pg type xml" });
      case "opaque": case "map": case "geometry": case "zoned_datetime":
        return POSTGRES_IN_JSON[name] ?? (name ? { schema: { description: `pg type ${name}` }, type: JSON_TYPE } : ANY);
    }
  }
  if (dialect === "mysql") {
    // MySQL writes a SET as its members joined by commas, and any bytes as their base64.
    if (isMysqlSet(t)) return string();
    if (t.kind === "bytes" || (t.kind === "array" && name === "bit")) return MYSQL_BYTES;
  }
  switch (t.kind) {
    case "boolean": return BOOLEAN;
    // An integer is a JSON number in there, a MySQL `tinyint(1)` too.
    case "integer": return { schema: integer(t), type: "number" };
    case "float":
    case "decimal": {
      const number = { type: "number", format: t.kind === "decimal" ? "decimal" : t.exponent_bits <= 8 ? "float" : "double" };
      const text = [...(t.nan ? ["NaN"] : []), ...(t.infinity ? ["Infinity", "-Infinity"] : [])];
      if (dialect === "postgres" && !inUnion) return { schema: number, type: "number" };
      return { schema: dialect === "postgres" && text.length ? { anyOf: [number, { enum: text }] } : number, type: ["number", ...text.map((x) => JSON.stringify(x))].join(" | ") };
    }
    case "text": return string();
    case "uuid": return string(uuid(t));
    case "bytes": return string();
    case "date": return string(besideFormat({ type: "string", format: "date" }, t, true));
    case "instant":
      // A MySQL TIMESTAMP is the session's wall-clock time.
      return string(dialect === "mysql" ? WALL_CLOCK : besideFormat({ type: "string", format: "date-time" }, t, true));
    case "datetime": return string(WALL_CLOCK);
    case "time": return string(timeOfDay(t, name));
    case "duration": return string(MYSQL_TIME);
    case "range": case "multirange": case "xml": return string({ type: "string", description: `${dialect === "mysql" ? "MySQL" : "ClickHouse"} type ${name || t.kind}` });
    case "array": case "struct": case "union": case "enum": case "tuple": case "json": case "table": case "intersection": case "tagged_union": case "null": case "unknown": return undefined;
    default: return ANY;
  }
}

/** An integer's JSON Schema: OpenAPI's `int32` or `int64` where its range fits one (an unsigned type needs a bit more), and no format past 64 bits, as ClickHouse's UInt64 and wider reach. A SQLite INTEGER is 64 bits wide. */
function integer(t: EngineType, dialect?: Dialect): Schema {
  const bits = dialect === "sqlite" || t.kind !== "integer" ? 64 : t.bits + (t.unsigned ? 1 : 0);
  return bits <= 32 ? { type: "integer", format: "int32" } : bits <= 64 ? { type: "integer", format: "int64" } : { type: "integer" };
}

/** RFC 9562's text form, its version digit and its variant fixed where every value has a version. */
function uuid(t: Extract<EngineType, { kind: "uuid" }>): Schema {
  if (t.versions === undefined) return { type: "string", format: "uuid" };
  const digits = t.versions.map((v) => v.toString(16)).join("");
  return { type: "string", format: "uuid", pattern: `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-${digits.length === 1 ? digits : `[${digits}]`}[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$` };
}

/** A date and a time with no zone, which RFC 3339's date-time does not admit. */
const WALL_CLOCK: Schema = { type: "string", description: "a date and time without a time zone, as the database wrote it (2026-10-20T10:00:00.123456)" };

/** MySQL's TIME, a length of time rather than a time of day. */
const MYSQL_TIME: Schema = { type: "string", description: "a length of time as MySQL writes it, up to 838:59:59 either way (-12:00:00.5)" };

/** RFC 3339's time, which JSON Schema's `time` format names, needs an offset and stops at 23:59:60. */
function timeOfDay(t: Extract<EngineType, { kind: "time" }>, name: string): Schema {
  return t.offset ? { type: "string", description: `pg type ${name}` } : { type: "string", description: "a time of day without a time zone (10:00:00.5)" };
}

/** A SQLite value's storage class, as the affinity of the declared type the engine traced it to names it, or of the value it computes. */
function sqliteClass(t: EngineType): string {
  const name = "label" in t ? (t.label ?? "") : "";
  switch (t.kind) {
    case "boolean": return "bool";
    case "integer": return sqliteWide(name) ? "int8" : "integer";
    case "float": return "real";
    case "text": return "text";
    case "bytes": return "blob";
    case "union": {
      if (/^bool(ean)?$/i.test(name.trim())) return "bool";
      const affinity = sqliteAffinity(name);
      return affinity === "integer" && sqliteWide(name) ? "int8" : affinity;
    }
    default: return "";
  }
}

/** A value of a type with no structure of its own here, outside any document: as a parameter the caller passes (`input`), or as `.run()` returns it. */
function scalar(t: EngineType, dialect: Dialect, input: boolean): Schema {
  const name = bareName(t);
  if (dialect === "sqlite") {
    // A SQLite INTEGER is a number as a parameter (src/render.ts); as `.run()` returns it, its kind's.
    if (input) {
      switch (sqliteClass(t)) {
        case "integer": case "int8": return { type: "integer", format: "int64" };
        case "real": return { type: "number", format: "double" };
        case "text": return { type: "string" };
        case "blob": return { type: "string", format: "byte", contentEncoding: "base64" };
        case "numeric": return { type: ["number", "string"] };
        case "bool": return { type: "boolean" };
        default: return {};
      }
    }
    const kinds = columnKind(t, dialect).split("|");
    if (kinds.length > 1) {
      // A NUMERIC: whichever storage class the row stored, an integer being a number too.
      const types = [...new Set(kinds.map((k) => kindSchema(scalarKind(k), t, dialect).type as string))];
      return { type: types.includes("number") ? types.filter((type) => type !== "integer") : types };
    }
    return kindSchema(scalarKind(kinds[0]!), t, dialect);
  }
  if (t.kind === "union" && !(dialect === "postgres" && (name === "inet" || name === "cidr"))) return anyOf(t.members.map((m) => scalar(m, dialect, input)));
  const kind = columnKind(t, dialect);
  if (input) {
    // A MySQL TINYINT(1) or BIT(1) column's parameter takes the truth value its rows return, or the integer it stores.
    if (dialect === "mysql" && kind === "bool" && t.kind !== "boolean") return { anyOf: [{ type: "boolean" }, { type: "integer", format: "int32" }] };
    // What every driver binds: a 64-bit integer as a number, a BigInt or its digits; a decimal as its digits.
    // A ClickHouse parameter is written by Reylo, which takes a number or a BigInt and no digits.
    if (kind === "int8") return dialect === "clickhouse" ? kindSchema("int8", t, dialect) : { ...kindSchema("int8", t, dialect), type: ["integer", "string"] };
    if (kind === "decimal") return { type: "string", format: "decimal" };
  }
  if (kind.includes("|")) return { anyOf: kind.split("|").map((k) => kindSchema(scalarKind(k), t, dialect)) };
  const schema = kindSchema(scalarKind(kind), t, dialect);
  if (kind !== "text") return besideFormat(schema, t, false);
  if (isMysqlSet(t)) return { type: "string" };
  switch (t.kind) {
    case "text": return { type: "string" };
    case "uuid": return uuid(t);
    case "time": return timeOfDay(t, name);
    case "duration": return MYSQL_TIME;
    default: return { type: "string", description: `${dialect === "mysql" ? "MySQL" : dialect === "clickhouse" ? "ClickHouse" : "pg"} type ${name || t.kind}` };
  }
}

/** A union's schema: any of its members', and none for a union of no members, which JSON Schema has no empty `anyOf` for. */
function anyOf(members: (Schema | JsonSchema)[]): Schema {
  return members.length ? { anyOf: members } : NOTHING;
}

/** `kind`, a column's kind with no structure of its own (src/values.ts), as the scalar kind it is. */
function scalarKind(kind: string): ScalarKind {
  if (!Object.hasOwn(TS_TYPE, kind)) throw new Error(`Reylo has no JSON Schema for a value of the kind ${kind} here; this is a bug in Reylo, which writes the schema of every kind a value takes`);
  return kind as ScalarKind;
}

// The fields of a date's and an instant's text, each within its range: a year from 0001 to 9999, a
// month and a day, a time of day, and an offset from UTC within RFC 3339's 23:59 where the value
// stands on its own. The same in engine/core.
const YEAR = "(000[1-9]|00[1-9][0-9]|0[1-9][0-9]{2}|[1-9][0-9]{3})";
const MONTH_DAY = "-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])";
const CLOCK = "T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]+)?";
const OFFSET = "[+-]([01][0-9]|2[0-3]):[0-5][0-9]";
/** ISO 8601's extended year of a value outside 0000–9999, as `isoYear` writes it: a sign and six digits or more, never `-000000`. */
const EXTENDED_YEAR = `([+-](0[1-9][0-9]{4}|[1-9][0-9]{5,})|-00${YEAR})`;
/**
 * An instant's time as Postgres writes it inside a document, in the session's zone: its offset to the
 * second before the zone's first standard time, and as far as 169 hours under a POSIX zone
 * (`XXX-167:59:60YYY` in its summer), whose offset Postgres reads up to 168 hours and summer time
 * moves one more; the hours in two digits below 100, as Postgres pads them.
 */
const DOCUMENT_TIME = `${CLOCK}[+-](([0-9]{2}|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00)`;
/** Of those offsets, the ones RFC 3339's refuses: to the second, or 24 hours or more. A four-digit year with any other is the format's, which checks the calendar too. */
const ODD_OFFSET = "[+-](([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]|(2[4-9]|[3-9][0-9]|1[0-5][0-9]|16[0-8]):[0-5][0-9](:[0-5][0-9])?|169:00)";

/**
 * A year that RFC 3339's four digits do not write, as a date's or an instant's text has it: ISO
 * 8601's extended year where the run time writes the value (`isoYear`, and a Temporal value's
 * JSON), and inside a document Postgres's own, five digits or more, or four with BC after the
 * value (`12345-01-01`, `0002-06-01 BC`). Inside a document an instant of a year from 1 to 9999
 * takes Postgres's offset too where the format refuses it (`1849-12-31T19:03:58-04:56:02`,
 * `ODD_OFFSET`). The same in engine/core.
 */
const PAST_YEARS = {
  date: [`^${EXTENDED_YEAR}${MONTH_DAY}$`, `^([1-9][0-9]{4,}${MONTH_DAY}|${YEAR}${MONTH_DAY} BC)$`],
  instant: [`^${EXTENDED_YEAR}${MONTH_DAY}${CLOCK}(Z|${OFFSET})$`, `^([1-9][0-9]{4,}${MONTH_DAY}${DOCUMENT_TIME}|${YEAR}${MONTH_DAY}${DOCUMENT_TIME} BC|${YEAR}${MONTH_DAY}${CLOCK}${ODD_OFFSET})$`],
} as const;

/** The text of a year outside 0000–9999, which a bound with a sign says a date or an instant of `t` reaches, standing where `inJson` says. */
function pastYears(t: EngineType, inJson: boolean): Schema | undefined {
  if (t.kind !== "date" && t.kind !== "instant") return undefined;
  const signed = [t.min, t.max, t.exclusive_min, t.exclusive_max].some((bound) => bound !== undefined && /^[+-]/.test(bound));
  return signed ? { type: "string", pattern: PAST_YEARS[t.kind][inJson ? 1 : 0] } : undefined;
}

/**
 * A date's or a timestamp's format and, beside it, the text of each value of `t` it refuses: a year
 * outside 0000–9999 (`pastYears`), and infinity, which `.run()` gives, and Postgres writes inside
 * a document, as the text `infinity` or `-infinity`.
 */
function besideFormat(schema: Schema, t: EngineType, inJson: boolean): Schema {
  if (schema.format === undefined) return schema;
  const past = pastYears(t, inJson);
  const infinite = (t.kind === "date" || t.kind === "instant" || t.kind === "datetime" || t.kind === "zoned_datetime") && t.infinity === true;
  const forms = [schema, ...(past === undefined ? [] : [past]), ...(infinite ? [{ enum: ["infinity", "-infinity"] }] : [])];
  return forms.length > 1 ? { anyOf: forms } : schema;
}

/** The JSON form of a value of `kind` as `.run()` returns it, once JSON: each kind's one representation (src/values.ts). */
function kindSchema(kind: ScalarKind, t: EngineType, dialect: Dialect): Schema {
  switch (kind) {
    case "bool": return { type: "boolean" };
    // A SQLite INTEGER is 64 bits wide.
    case "int": return integer(t, dialect);
    case "float": return t.kind === "float" && t.exponent_bits <= 8 ? { type: "number", format: "float" } : { type: "number", format: "double" };
    // A BigInt, as the JSON integer it is.
    case "int8": return integer(t);
    case "decimal":
    case "money": return { type: "string", format: "decimal" };
    case "timestamptz": return { type: "string", format: "date-time" };
    // RFC 3339's duration, which JSON Schema's `duration` format names, has no sign and no fraction.
    case "interval": return { type: "string", description: "an ISO 8601 duration with its sign in front (P1Y2M3DT4H5M6.789S, -P1D); one whose parts differ in sign signs each part (P1M-1D)" };
    case "date": return { type: "string", format: "date" };
    case "timestamp": return WALL_CLOCK;
    case "bytes": return { type: "string", format: "byte", contentEncoding: "base64" };
    case "vector": return { type: "array", items: { type: "number", format: "float" } };
    case "hstore": return { type: "object", additionalProperties: { type: ["string", "null"] } };
    case "text": return { type: "string" };
    case "json": return {};
    case "geometry": return {};
    case "unknown": return {};
    case "enum": return { type: "string" };
    // A Set's members, which a Set holds once each.
    case "set": return { type: "array", items: { type: "string" }, uniqueItems: true };
    // A geometric type is an object of its fields, which `typeSchema` writes from its descriptor.
    case "point": case "line": case "lseg": case "box": case "path": case "polygon": case "circle": return { type: kind === "lseg" || kind === "polygon" ? "array" : "object" };
    default: {
      const unhandled: never = kind;
      throw new Error(`Reylo has no JSON Schema for a value of the kind ${String(unhandled)}`);
    }
  }
}

/** An object of `fields`, each spelled by `field`, every key required unless `optional` says otherwise. */
function object(fields: readonly { name: string }[], field: (f: number) => Schema | JsonSchema, optional: (f: number) => boolean = () => false, closed: boolean | Schema | JsonSchema = false): Schema {
  const properties: Record<string, unknown> = {};
  const required: string[] = [];
  // A later field of a name replaces an earlier one, as a row object and `JSON.parse` keep the last.
  // A field named `__proto__` is a key like any other, which an assignment would not make.
  fields.forEach((f, i) => {
    Object.defineProperty(properties, f.name, { value: field(i), writable: true, enumerable: true, configurable: true });
    const at = required.indexOf(f.name);
    if (at >= 0) required.splice(at, 1);
    if (!optional(i)) required.push(f.name);
  });
  // `closed`: no other key, or the schema of every other key.
  return { type: "object", properties, required, ...(closed === false ? {} : { additionalProperties: closed === true ? false : closed }) };
}

/** The JSON Schema of a value of `t`, as a result column (`input` false) or a parameter the caller passes (`input` true), or a structured parameter's member (`inRow`); `members`, a structured parameter's column list as written. */
export function typeSchema(t: EngineType, dialect: Dialect, input = false, inJson = false, inRow = false, members?: readonly { written: string; schema?: JsonSchema }[], codecs: Codecs = {}): Schema | JsonSchema {
  const schema = plainSchema(t, dialect, input, inJson, inRow, members, codecs);
  // A value a codec gives another type stands by that type's form.
  return !inJson && leafCodec(t, dialect, codecs) !== undefined ? schema : withFacts(schema, t, input);
}

/**
 * A number the format writes as text, where a JSON number holds that very decimal: a reader takes a
 * number in a schema as the decimal it prints as, so one a double rounds, as 0.29999999999999999 to
 * 0.3 or 9007199254740993 to its neighbour, would hold a value to another bound.
 */
function exactNumber(text: string, whole: boolean): number | undefined {
  const value = Number(text);
  if (whole) return Number.isSafeInteger(value) ? value : undefined;
  const written = decimal(text);
  return Number.isFinite(value) && written !== undefined && decimal(String(value)) === written ? value : undefined;
}

/** A divisor the format writes as text, where a JSON number holds it exactly (`exactNumber`): one a double rounds, as 1e-400 to zero, would hold a value to another divisor. */
function exactDivisor(text: string, whole: boolean): number | undefined {
  const value = exactNumber(text, whole);
  return value === 0 ? undefined : value;
}

/**
 * Whether a double holds every value of `t` exactly, as a reader of JSON in JavaScript parses one:
 * an integer within ±2^53 by its width or its bounds, a decimal of at most 15 digits, and a float of
 * a double's precision, which the database writes at the shortest text that reads back as it.
 * Elsewhere a value rounds, so that a true multiple of a divisor, or a value just past an exclusive
 * bound, reads as none.
 */
function exactInDouble(t: Extract<EngineType, { kind: "integer" | "float" | "decimal" }>): boolean {
  switch (t.kind) {
    case "integer": return t.bits + (t.unsigned ? 1 : 0) <= 54 || [t.min, t.max].every((b) => b !== undefined && Number.isSafeInteger(Number(b)));
    case "decimal": return t.precision !== undefined && t.precision <= 15;
    case "float": return t.mantissa_bits >= 52;
  }
}

/** A decimal's text as its digits and power of ten, with no zero to spare, as `1.50` and `15e-1` both read `15e-1`. */
function decimal(text: string): string | undefined {
  const m = /^[+-]?(\d*)(?:\.(\d*))?(?:e([+-]?\d+))?$/i.exec(text.trim());
  const fraction = m?.[2] ?? "";
  if (m === null || m[1]! + fraction === "") return undefined;
  const all = (m[1]! + fraction).replace(/^0+/, "");
  let end = all.length;
  while (end > 0 && all[end - 1] === "0") end--;
  const digits = all.slice(0, end);
  if (digits === "") return "0";
  return `${digits}e${Number(m[3] ?? 0) - fraction.length + all.length - digits.length}`;
}

/**
 * An I-Regexp (RFC 9485), which matches a whole value, as the ECMAScript expression JSON Schema's
 * `pattern` reads: anchored at both ends, each `.` outside a class the `[^\n\r]` I-Regexp means by
 * it (RFC 9485 §5.3), and a `^` or a `$`, which I-Regexp reads as itself, escaped
 * (`ecma_pattern` in engine/core).
 */
export function ecmaPattern(source: string): string {
  let out = "";
  let escaped = false;
  let inClass = false;
  for (const c of source) {
    if (escaped) {
      out += c;
      escaped = false;
    } else if (c === "\\") {
      out += c;
      escaped = true;
    } else if (c === "[" && !inClass) {
      out += c;
      inClass = true;
    } else if (c === "]" && inClass) {
      out += c;
      inClass = false;
    } else if (c === "." && !inClass) out += "[^\\n\\r]";
    else if ((c === "^" || c === "$") && !inClass) out += `\\${c}`;
    else out += c;
  }
  return `^(?:${out})$`;
}

/**
 * `schema` with the facts of `t` JSON Schema says the same way, each only where the value's form here
 * makes it true: a text's size in code points and its pattern on a string, a number's bounds and
 * divisor on a JSON number, within what a JSON number writes exactly, an array's size on an
 * array, and a map's size, which counts its keys, on an object. A decimal given as its text takes
 * no bounds. A text parameter's exact size is a maximum, since `char(n)` pads a shorter value. A
 * size in code units or bytes has no equivalent. A text's format is never written: it says nothing
 * of the values, and a validator asserts a format it knows.
 */
function withFacts(schema: Schema | JsonSchema, t: EngineType, input: boolean): Schema | JsonSchema {
  if (typeof schema !== "object" || schema === null) return schema;
  const facts: Schema = {};
  const sized = (size: { exact?: number; min?: number; max?: number }, low: string, high: string, atMost: boolean) => {
    const min = atMost ? size.min : (size.exact ?? size.min);
    const max = size.exact ?? size.max;
    if (min !== undefined) facts[low] = min;
    if (max !== undefined) facts[high] = max;
  };
  if (t.kind === "text" && schema.type === "string") {
    if (t.size !== undefined && !("unit" in t.size && t.size.unit !== undefined)) sized(t.size, "minLength", "maxLength", input);
    if (t.pattern !== undefined) facts.pattern = ecmaPattern(t.pattern);
  }
  // A number's bounds and divisor where a double reading the value agrees with the value: an
  // inclusive bound a JSON number writes exactly holds of a value rounded towards it too, but an
  // exclusive bound or a divisor only of a value held exactly (`exactInDouble`). A float narrower
  // than a double takes none: the database writes its shortest text, which reads as another double.
  if ((t.kind === "integer" || t.kind === "float" || t.kind === "decimal") && (schema.type === "integer" || schema.type === "number") && (t.kind !== "float" || exactInDouble(t))) {
    const whole = t.kind === "integer";
    const exact = exactInDouble(t);
    const bound = (text: string | undefined, keyword: string) => {
      const value = text === undefined ? undefined : exactNumber(text, whole);
      if (value !== undefined) facts[keyword] = value;
    };
    bound(t.min, "minimum");
    bound(t.max, "maximum");
    if (!whole && exact) {
      bound(t.exclusive_min, "exclusiveMinimum");
      bound(t.exclusive_max, "exclusiveMaximum");
    }
    const divisor = t.kind === "float" || t.multiple_of === undefined || !exact ? undefined : exactDivisor(t.multiple_of, whole);
    if (divisor !== undefined) facts.multipleOf = divisor;
  }
  if (t.kind === "array" && schema.type === "array" && t.size !== undefined) sized(t.size, "minItems", "maxItems", false);
  if (t.kind === "map" && schema.type === "object" && t.size !== undefined) sized(t.size, "minProperties", "maxProperties", false);
  return Object.keys(facts).length ? { ...schema, ...facts } : schema;
}

/**
 * `schema` with a comment as its description. A schema the statement's author wrote keeps its own
 * description; one the type gave (a timestamp's format, say) sits beneath the comment, as it does
 * beside null.
 */
function described(schema: Schema | JsonSchema, comment: string | undefined, written = false): Schema | JsonSchema {
  if (comment === undefined) return schema;
  if (schema === true) return { description: comment };
  if (typeof schema !== "object" || schema === null) return schema;
  return !written && "description" in schema ? { description: comment, allOf: [schema] } : { description: comment, ...schema };
}

function plainSchema(t: EngineType, dialect: Dialect, input: boolean, inJson: boolean, inRow: boolean, members: readonly { written: string; schema?: JsonSchema }[] | undefined, codecs: Codecs): Schema | JsonSchema {
  // A family the project file's `js.codecs` gives another type is that type's JSON form, outside JSON.
  const codec = inJson ? undefined : leafCodec(t, dialect, codecs);
  if (codec === "set:array" && t.kind === "array" && isMysqlSet(t)) return { ...CODEC_SCHEMA[codec], items: typeSchema(t.element.type, dialect, input) };
  // A geometry parameter may leave out its srid, which then is the column's.
  if (codec === "geometry" && input) return { ...CODEC_SCHEMA.geometry, required: ["type"] };
  if (codec === "date" || codec === "instant") {
    // A Temporal date's or instant's JSON writes a year outside 0000–9999 as ISO 8601 extends it.
    const past = pastYears(t, false);
    if (past !== undefined) return { anyOf: [CODEC_SCHEMA[codec], past] };
  }
  if (codec !== undefined) return CODEC_SCHEMA[codec];
  const name = bareName(t);
  // What the caller passes for a structured parameter: each key as a parameter of its type takes it.
  if (members !== undefined) {
    const record = t.kind === "struct" ? t : t.kind === "array" && t.element.type.kind === "struct" ? t.element.type : undefined;
    if (record !== undefined) {
      // A JSON member's schema as written, with its defaults and formats.
      const row = object(record.fields, (i) => (record.fields[i]!.nullable ? nullable : (s: Schema | JsonSchema) => s)(members[i]?.schema ?? typeSchema(record.fields[i]!.type, dialect, true, false, true, undefined, codecs)), (i) => record.fields[i]!.optional === true, true);
      return t.kind === "struct" ? row : { type: "array", items: row };
    }
  }
  // A value inside a document as the database writes it there (`inDocument`), the declared type's table too.
  const form = inJson ? inDocument(t, dialect) : undefined;
  if (form !== undefined) return form.schema;
  const slot = (s: { type: EngineType; nullable?: true }) => (s.nullable ? nullable : (x: Schema | JsonSchema) => x)(typeSchema(s.type, dialect, input, inJson, false, undefined, codecs));
  switch (t.kind) {
    // An enum's values as its base's form writes them: a truth value and a number JSON writes
    // exactly as themselves, text as itself, and any other value (a decimal's text, a 64-bit
    // integer's digits) by its base alone.
    case "enum": {
      const base = typeSchema(t.type, dialect, input, inJson, inRow, undefined, codecs);
      if (typeof base !== "object" || base === null) return base;
      const values = t.values.map(({ value: v }): unknown => {
        if (base.type === "boolean") return v === "true";
        if (base.type === "integer" || base.type === "number") return exactNumber(v, base.type === "integer");
        if (base.type === "string" && t.type.kind === "text") return v;
        return undefined;
      });
      return values.every((v) => v !== undefined) ? { ...base, enum: values } : base;
    }
    // A tuple: its elements, then its optional ones, then any number of its rest.
    // JSON Schema 2020-12 refuses an empty `prefixItems`, so a tuple of none is `items` alone.
    case "tuple": {
      const items = [...t.elements, ...(t.optional_elements ?? [])].map(slot);
      return { type: "array", ...(items.length ? { prefixItems: items } : {}), items: t.rest ? slot(t.rest) : false, minItems: t.elements.length, ...(t.rest ? {} : { maxItems: items.length }) };
    }
    // A table nested in a value is an array of its rows.
    case "table":
      return { type: "array", items: object(t.columns, (i) => slot(t.columns[i]!)) };
    // Of no members, every value.
    case "intersection":
      return t.members.length ? { allOf: t.members.map((m) => typeSchema(m, dialect, input, inJson, inRow, undefined, codecs)) } : {};
    // A union inside a document: any of its members, each as the database writes it there.
    case "union":
      if (inJson) return anyOf(t.members.map((m) => inDocument(m, dialect, true)?.schema ?? typeSchema(m, dialect, input, true, inRow, undefined, codecs)));
      break;
    case "null":
      return { type: "null" };
    case "array": {
      // A MySQL SET's members, each one of its labels where the column declares them.
      if (isMysqlSet(t)) {
        if (inJson || dialect !== "mysql") break;
        return { type: "array", items: typeSchema(t.element.type, dialect, input), uniqueItems: true };
      }
      // A type a client gets as its own (a bit string, a vector) is no array here; a segment and a
      // polygon are arrays of their points. Read outside a document only: a row's kind doubles in
      // length with each level it nests, so a document, which may nest hundreds deep, never builds one.
      if (!inJson) {
        const kind = columnKind(t, dialect);
        if (!kind.endsWith("[]") && kind !== "lseg" && kind !== "polygon") break;
      }
      // A result's elements may be null unless the statement shows they are not; a parameter's
      // are strict, but a structured parameter's member keeps them (src/render.ts). A ClickHouse
      // array's type says whether an element may be null, a parameter's too.
      const each = typeSchema(t.element.type, dialect, input, inJson, inRow, undefined, codecs);
      const items = (input && dialect !== "clickhouse" ? inRow : t.element.nullable) ? nullable(each) : each;
      // An array the project lets nest holds its elements, or arrays of them at any depth.
      return { type: "array", items: !input && nestsAnyDeep(t) ? { anyOf: [items, { type: "array" }] } : items };
    }
    // A row value is an object of its fields, and so is a geometric type outside a document; an open
    // one's other keys are each of its `extra`.
    case "struct":
      return object(t.fields, (i) => (t.fields[i]!.nullable ? nullable : (s: Schema | JsonSchema) => s)(typeSchema(t.fields[i]!.type, dialect, input, inJson, false, undefined, codecs)), undefined, t.extra === undefined ? false : slot(t.extra));
    case "range":
    case "multirange": {
      // A Postgres range is an object of its bounds outside a document.
      if (inJson || dialect !== "postgres") break;
      const bound = typeSchema(t.subtype, dialect, input, false, false, undefined, codecs);
      const range: Schema = { anyOf: [{ type: "object", properties: { empty: { const: true } }, required: ["empty"], additionalProperties: false }, { type: "object", properties: { lower: bound, upper: bound, bounds: { enum: [...RANGE_BOUNDS] } }, required: input ? [] : ["bounds"], additionalProperties: false }] };
      return t.kind === "range" ? range : { type: "array", items: range };
    }
    case "json":
      // What a statement built: its keys and items are inside the document.
      if (t.content !== undefined) {
        const content = typeSchema(t.content.type, dialect, input, true);
        return t.content.nullable ? nullable(content) : content;
      }
      return schemaOf(t) ?? {};
    case "tagged_union":
      // A tagged union as its tag and its value; inside a document, whose form of it the engine
      // does not read, any JSON.
      if (inJson) return {};
      return anyOf(t.alternatives.map((a) => object([{ name: "tag" }, { name: "value" }], (i) => (i === 0 ? { const: a.tag } : (a.nullable ? nullable : (s: Schema | JsonSchema) => s)(typeSchema(a.type, dialect, input, false, false, undefined, codecs))))));
    case "unknown":
      return {};
  }
  // Inside a document, a value of no form the database is known to write there is any JSON.
  return inJson ? {} : scalar(t, dialect, input);
}

/** The query's input and output schemas: the parameters as an object, and the result as `run` returns it once JSON. */
/** A decimal library's value as JSON: its `toJSON()` writes its `toString()`, which turns to exponent form for a large or small value. */
const LIBRARY_DECIMAL: Schema = { type: "string", format: "decimal", description: "a decimal as its library's toJSON() writes it, in exponent form for a large or small value (1e+40, 1e-7)" };

/** A typed array of numbers as JSON: an array of them, as a Set is one of its members (`AsJson` in src/index.ts). */
const TYPED_ARRAY: Schema = { type: "array", items: { type: "number" } };

/** The JSON form of each codec's value: what `JSON.stringify` writes of it, a Temporal value's being its ISO 8601 text, which a SQLite column declared as a date gives where it otherwise holds any storage class. */
const CODEC_SCHEMA: Readonly<Record<Codec, Schema>> = {
  "decimal:number": { type: "number" },
  "decimal:decimal.js": LIBRARY_DECIMAL,
  "decimal:bignumber.js": LIBRARY_DECIMAL,
  "decimal:big.js": LIBRARY_DECIMAL,
  "bytes:string": { type: "string", format: "byte", contentEncoding: "base64" },
  date: { type: "string", format: "date" },
  time: { type: "string", description: "a time of day without a time zone (10:00:00.5)" },
  datetime: { type: "string", description: "a date and time without a time zone (2026-10-20T10:00:00.123456)" },
  instant: { type: "string", format: "date-time" },
  // RFC 3339's duration, which JSON Schema's `duration` format names, has no sign.
  duration: { type: "string", description: "an ISO 8601 duration with its sign in front (P1Y2M3DT4H5M6.789S, -P1D)" },
  // A SET's members, each once; `typeSchema` gives its items the members' own schema.
  "set:array": { type: "array", items: { type: "string" }, uniqueItems: true },
  "vector:float32": TYPED_ARRAY,
  "vector:float16": TYPED_ARRAY,
  // ip-address's values have no toJSON(), so JSON.stringify writes their fields.
  "inet:ip-address": { type: "object", properties: { address: { type: "string" }, subnetMask: { type: "integer" } }, description: "an ip-address Address4 or Address6, which JSON.stringify writes as its fields ({\"address\":\"10.0.0.1/8\",\"subnetMask\":8,…})" },
  geometry: { type: "object", properties: { type: { type: "string" }, srid: { type: "integer", minimum: 0 } }, required: ["type", "srid"], description: "a GeoJSON geometry, with its SRID as srid" },
};

export function querySchemas(analysis: Analysis, dialect: Dialect, codecs: Codecs = {}): { input: Schema; output: Schema } {
  // A typed JSON parameter's schema as written, with its defaults; one with a `default` at its root may be left out, as may one that takes null.
  const input = object(analysis.params, (i) => {
    const p = analysis.params[i]!;
    const s = p.schema ?? typeSchema(p.type, dialect, true, false, false, p.members, codecs);
    return described(p.nullable ? nullable(s) : s, p.comment, p.schema !== undefined);
  }, (i) => {
    const p = analysis.params[i]!;
    const s = p.schema;
    return p.nullable === true || p.optional === true || (s !== null && typeof s === "object" && s !== undefined && "default" in s);
  }, true);
  const column = (c: Analysis["columns"][number]) => {
    const s = typeSchema(c.type, dialect, false, false, false, undefined, codecs);
    return described(c.nullable ? nullable(s) : s, c.comment);
  };
  // A statement whose one column has no name gives that column's values.
  const value = analysis.value ? analysis.columns[0] : undefined;
  const row = value ? (column(value) as Schema) : object(analysis.columns, (i) => column(analysis.columns[i]!));
  const output: Schema = analysis.cardinality === "many" ? { type: "array", items: row } : analysis.cardinality === "one" ? row : analysis.cardinality === "at_most_one" ? (value?.nullable ? row : { anyOf: [row, { type: "null" }] }) : { type: "null" };
  const about = analysis.comment === undefined ? {} : { description: analysis.comment };
  return { input: { $schema: "https://json-schema.org/draft/2020-12/schema", ...about, ...input }, output: { $schema: "https://json-schema.org/draft/2020-12/schema", ...about, ...output } };
}
