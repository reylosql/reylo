// What each database writes inside a JSON document it builds, as the TypeScript type of that value
// (src/render.ts): a value inside a document is the server's own JSON, which the run time decodes
// only where the type says a non-finite float or decimal stands (src/values.ts, `numbersInJson`).
import { bareName, isMysqlSet, type EngineType } from "./engine-type.ts";

type Dialect = "postgres" | "mysql" | "sqlite" | "clickhouse";

/** Any JSON value, as a module names it: the type `reylo` exports. */
export const JSON_TYPE = "Json";

/**
 * The Postgres types whose form inside a JSON document their kind does not say, and which Postgres
 * writes there as their text, having no cast to JSON: types the format structures (a bit string, a
 * vector, a geometric type, a network address), built-in system types, and the extensions' on Neon
 * and Supabase (PostGIS's others, cube, seg, isn, intarray, h3, hll, ip4r, pgx_ulid, prefix, rdkit,
 * roaringbitmap, semver, unit, VectorChord). The same list in engine/core/src/schema.rs, which
 * tests/render.test.ts holds this one to.
 */
export const POSTGRES_TEXT_IN_JSON: readonly string[] = [
  "bit", "varbit", "vector", "halfvec", "sparsevec",
  "point", "line", "lseg", "box", "path", "polygon", "circle",
  "inet", "cidr",
  "money", "tsvector", "tsquery", "regclass", "regtype",
  "ltree", "lquery", "ltxtquery",
  "oid", "pg_lsn", "jsonpath", "xid", "xid8", "oid8", "regdatabase",
  "regproc", "regprocedure", "regoper", "regoperator", "regnamespace", "regrole", "regcollation", "regconfig", "regdictionary",
  "tid", "cid", "aclitem", "refcursor", "pg_snapshot", "txid_snapshot",
  "geography", "box2d", "box3d", "spheroid", "raster",
  "cube", "seg", "ean13", "isbn", "isbn13", "ismn", "ismn13", "issn", "issn13", "upc",
  "query_int", "h3index", "hll", "hll_hashval",
  "ip4", "ip4r", "ip6", "ip6r", "ipaddress", "iprange", "ulid", "prefix_range",
  "mol", "qmol", "xqmol", "reaction", "bfp", "sfp", "roaringbitmap", "s3vec",
  "semver", "semverrange", "semvermultirange", "unit", "unitrange", "unitmultirange", "rabitq8", "rabitq4",
];

// What Postgres writes inside a JSON document for a type whose form there its kind does not say:
// the text of each type above, an object of its keys for an hstore, GeoJSON for a geometry, and an
// object of its columns for a row whose columns were not read. A type named nowhere here, whose form
// the engine does not know, is any JSON value: an extension's type may have a cast to JSON.
const POSTGRES_IN_JSON: ReadonlyMap<string, string> = new Map([
  ...POSTGRES_TEXT_IN_JSON.map((name): [string, string] => [name, "string"]),
  // An hstore is an object of its keys inside a document, each value text or null (`{"a": "1", "b": null}`).
  ["hstore", "Record<string, string | null>"],
  // PostGIS writes a geometry through its cast to JSON: a GeoJSON geometry, `{"type":"Point","coordinates":[1,2]}`.
  ["geometry", "{ type: string; [key: string]: Json | undefined }"],
  // A row whose columns were not read (a subquery's): an object of them inside a document.
  ["record", "Record<string, Json>"],
]);

/** pgvector's types, arrays of numbers that Postgres writes inside a document as their text, in whatever schema the extension was created in. */
const PGVECTOR = new Set(["vector", "halfvec", "sparsevec"]);

/**
 * The type of a value of `t` Postgres writes inside a document, of those the table above names. A
 * built-in structure (a geometric type, a bit string, a network address) is known by its own
 * label, which a type of another schema never has, since the engine qualifies one: a composite of
 * the user's named `public.point` is an object of its fields there. An extension's type is known
 * by its name in whatever schema it was created in.
 */
export function postgresInJson(t: EngineType): string | undefined {
  const name = bareName(t);
  const key = (t.kind === "array" || t.kind === "struct" || t.kind === "union") && !(t.kind === "array" && PGVECTOR.has(name)) ? ("label" in t ? t.label : undefined) : name;
  return key === undefined ? undefined : POSTGRES_IN_JSON.get(key);
}

/**
 * The type of a value of `t` that `dialect` writes inside a JSON document it builds, where the value
 * is no structure the writers walk. Undefined for an array, a row, a tuple, an enum, a union and a
 * document, each of whose parts it answers for. `inUnion`: the value is a union's member, where a
 * float's or a decimal's NaN and infinities stay the text Postgres writes, which the run time
 * converts only where the value stands alone (src/values.ts, `numbersInJson`).
 */
export function inDocument(t: EngineType, dialect: Dialect, inUnion = false): string | undefined {
  const name = bareName(t);
  // MySQL writes a SET as its members joined by commas.
  if (isMysqlSet(t)) return dialect === "sqlite" ? JSON_TYPE : "string";
  if (dialect === "sqlite") {
    // SQLite writes a value as its storage class: a number, text, or a NUMERIC's either; a column
    // declared BOOLEAN holds 0 and 1, which it writes as numbers. A BLOB is no JSON.
    switch (t.kind) {
      case "integer": case "float": case "decimal": return "number";
      case "boolean": return "boolean";
      case "text": return "string";
      // An INTEGER that may be a REAL, which SQLite keeps outside a STRICT table: a number of either kind.
      case "union": return /^bool(ean)?$/i.test(name.trim()) ? "number" : name.toUpperCase() === "NUMERIC" ? "number | string" : t.members.every((m) => m.kind === "integer" || m.kind === "float") ? "number" : JSON_TYPE;
      case "enum": case "array": case "tuple": case "struct": case "json": case "table": case "intersection": case "tagged_union": case "null": case "unknown": return undefined;
      default: return JSON_TYPE;
    }
  }
  if (dialect === "postgres") {
    switch (t.kind) {
      case "array": case "struct": case "union":
        // A type the format structures that Postgres writes as its text, or a network address.
        return t.kind === "struct" && name === "record" ? undefined : postgresInJson(t);
      // An oid is no number JSON writes: Postgres writes its text.
      case "integer": return name === "oid" ? "string" : "number";
      case "bytes": case "duration": case "range": case "multirange": case "xml": return "string";
      case "opaque": case "map": case "geometry": case "zoned_datetime": return postgresInJson(t) ?? JSON_TYPE;
    }
  }
  // MySQL writes any bytes as their base64.
  if (dialect === "mysql" && (t.kind === "bytes" || (t.kind === "array" && name === "bit"))) return "string";
  switch (t.kind) {
    case "boolean": return "boolean";
    // An integer is a JSON number in there, a MySQL `tinyint(1)` too.
    case "integer": return "number";
    case "float":
    case "decimal": {
      if (dialect === "postgres" && !inUnion) return "number";
      const text = [...(t.nan ? ["NaN"] : []), ...(t.infinity ? ["Infinity", "-Infinity"] : [])];
      return ["number", ...text.map((x) => JSON.stringify(x))].join(" | ");
    }
    case "text": case "uuid": case "bytes": case "date": case "instant": case "datetime": case "time": case "duration": case "range": case "multirange": case "xml": return "string";
    case "array": case "struct": case "union": case "enum": case "tuple": case "json": case "table": case "intersection": case "tagged_union": case "null": case "unknown": return undefined;
    default: return JSON_TYPE;
  }
}
