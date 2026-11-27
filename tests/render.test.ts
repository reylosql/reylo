import { test } from "node:test";
import assert from "node:assert/strict";
import type { EngineType } from "../src/engine.ts";
import { analysisWithNestedArrays } from "../src/engine-type.ts";
import { jsonNumbers, orNull, renderParamType, renderType } from "../src/render.ts";
import { columnKind, numbersInJson } from "../src/values.ts";
import { typeSchema } from "../src/schema.ts";
import { array, checked, doc, enumOf, field, lite, my, oneOf, pg, record } from "./descriptors.ts";

// A table's row type as the engine renders it once the catalog gave its columns.
const users: EngineType = record([field("id", pg("int8")), field("seen", pg("timestamptz"), true), field("tags", array(pg("text")), true), field("mood", enumOf("public.mood", ["ok", "sad"]))], "public.users");
// `json_build_object('author', u, 'posts', json_agg(p), 'n', count(*))`: a composite inside a document.
const object: EngineType = record([field("author", users), field("posts", { kind: "array", element: { type: users, nullable: true } }, true), field("n", pg("int8"))]);
const built = doc(object);

const inJson = `{ id: number; seen: string | null; tags: (string | null)[] | null; mood: "ok" | "sad" }`;

const onWire = `{ id: bigint; seen: string | null; tags: (string | null)[] | null; mood: "ok" | "sad" }`;

test("a composite is an object of its fields, each as a column of its type is, and a parameter takes the same object", () => {
  assert.equal(renderType(users, false, "postgres"), onWire);
  assert.equal(renderType(array(users), false, "postgres"), `(${onWire} | null)[]`);
  assert.equal(renderParamType(users, "postgres"), `{ id: number | bigint | string; seen: string | null; tags: (string | null)[] | null; mood: "ok" | "sad" }`);
  // A row whose fields are not known stays the text Postgres prints.
  assert.equal(renderType(pg("record"), false, "postgres"), "string");
});

test("a row value with two fields of one name is an object of the last, as .run() returns it, and a parameter takes the same object", () => {
  // `select s from (select 1 as a, 'x'::text as a) s` returns `{ s: { a: "x" } }`.
  const twice = record([field("a", pg("int4")), field("a", pg("text"))]);
  assert.equal(renderType(twice, false, "postgres"), "{ a: string }");
  assert.equal(renderParamType(twice, "postgres"), "{ a: string }");
  const tuple = record([field("a", { kind: "integer", bits: 32 }), field("a", { kind: "text" })]);
  assert.equal(renderType(tuple, false, "clickhouse"), "{ a: string }");
  assert.equal(renderParamType(tuple, "clickhouse"), "{ a: string }");
});

test("inside a JSON document Postgres 19's oid8 and regdatabase are their text, as Postgres writes them", () => {
  assert.equal(renderType(pg("oid8"), true, "postgres"), "string");
  assert.equal(renderType(pg("regdatabase"), true, "postgres"), "string");
});

test("inside a JSON document a composite is an object of its JSON-encoded fields", () => {
  assert.equal(renderType(users, true, "postgres"), inJson);
  // The renderer parenthesizes an element whose text holds a `|` anywhere, as it does for an object.
  assert.equal(renderType(array(users), true, "postgres"), `(${inJson} | null)[]`);
  // A constructor's fields are inside the document, at the top level too.
  const expected = `{ author: ${inJson}; posts: (${inJson} | null)[] | null; n: number }`;
  assert.equal(renderType(built, false, "postgres"), expected);
  assert.equal(renderType(object, true, "postgres"), expected);
});

test("one type per SQL type, on every driver: what the run time returns (src/values.ts)", () => {
  const wire = (name: string) => renderType(pg(name), false, "postgres");
  assert.deepEqual(["bool", "int2", "int4", "oid", "float4", "float8"].map((n) => wire(n)), ["boolean", "number", "number", "number", "number", "number"]);
  assert.equal(wire("int8"), "bigint");
  assert.deepEqual(["numeric", "money", "date", "timestamp", "timestamptz", "time", "timetz", "interval", "uuid", "text"].map((n) => wire(n)), Array(10).fill("string"));
  assert.equal(wire("bytea"), "Uint8Array");
  assert.equal(renderType({ kind: "json" }), "Json");
  // Every other type is its text: Reylo owns every oid, so no driver parses one.
  assert.deepEqual(["xid", "void", "record", "tsvector", "citext"].map((n) => wire(n)), Array(5).fill("string"));
  // An array of each, an enum's labels, a row value's array its text.
  assert.equal(renderType(array(pg("timestamptz"))), "(string | null)[]");
  assert.equal(renderType(array(pg("int8"))), "(bigint | null)[]");
  assert.equal(renderType(array(enumOf("public.mood", ["ok", "sad"]))), `("ok" | "sad" | null)[]`);
  // MySQL's: a BIGINT a BigInt, a DECIMAL its text, a DATETIME, a TIMESTAMP, a DATE and a TIME
  // their text, a BIT its bytes, and a truth value (a TINYINT(1), a BIT(1), a comparison) a boolean.
  const mysql = (t: EngineType) => renderType(t, false, "mysql");
  assert.equal(mysql(my("bigint")), "bigint");
  assert.deepEqual(["decimal", "datetime", "timestamp", "date", "time", "varchar"].map((n) => mysql(my(n))), Array(6).fill("string"));
  assert.equal(mysql(my("bit", 8)), "Uint8Array");
  assert.equal(mysql(my("tinyint(1)")), "boolean");
  assert.equal(mysql(my("bit", 1)), "boolean");
  assert.equal(mysql({ kind: "boolean" }), "boolean");
});

test("a union is either", () => {
  // What MySQL answers with by what is bound for a placeholder: a BIGINT is a BigInt, a DOUBLE a
  // number; an INT a temporary table may widen to a BIGINT is a BigInt either way.
  assert.equal(renderType(oneOf(my("bigint"), my("double")), false, "mysql"), "bigint | number");
  assert.equal(renderType(oneOf(my("bigint"), my("int")), false, "mysql"), "bigint");
  assert.equal(renderType(oneOf(my("double"), my("decimal")), false, "mysql"), "number | string");
  assert.equal(renderType(oneOf(my("date"), my("datetime")), false, "mysql"), "string");
  assert.equal(renderType(array(oneOf(my("double"), my("decimal"))), false, "mysql"), "(number | string | null)[]");
  // A truth value a document may hold as `true` or as 1.
  assert.equal(renderType(oneOf({ kind: "boolean" }, my("bigint")), true, "mysql"), "boolean | number");
  assert.equal(renderType(doc({ kind: "array", element: { type: oneOf({ kind: "boolean" }, my("bigint")), nullable: true } }), false, "mysql"), "(boolean | number | null)[]");
  // A member whose JSON form is unknown is nothing known inside a document.
  assert.equal(renderType(oneOf(my("bigint"), { kind: "opaque", label: "mystery" }), true, "mysql"), "Json");
  // A ClickHouse Variant is whichever of its types its value has, each as a column of that type
  // is, an array among them; one member of no known type leaves any value.
  const bytes = array({ kind: "integer", bits: 8, unsigned: true }, false);
  assert.equal(renderType(oneOf(bytes, { kind: "text" }), false, "clickhouse"), "number[] | string");
  assert.equal(renderType(oneOf(bytes, { kind: "unknown" }), false, "clickhouse"), "unknown");
  // A codec reaches a Variant whose members are all of its family, and no member of any other.
  const decimal = (precision: number): EngineType => ({ kind: "decimal", precision, min_scale: 2, max_scale: 2 });
  assert.equal(renderType(oneOf(decimal(9), decimal(18)), false, "clickhouse", { decimal: "number" }), "number");
  assert.equal(renderType(oneOf(array(decimal(9), false), { kind: "text" }), false, "clickhouse", { decimal: "number" }), "string[] | string");
});

test("pgvector's vector and halfvec are arrays of numbers, as columns and as parameters; inside a document their text", () => {
  for (const name of ["vector", "halfvec"]) {
    assert.equal(renderType(pg(name), false, "postgres"), "number[]");
    assert.equal(renderType(pg(name), true, "postgres"), "string");
    assert.equal(renderParamType(pg(name), "postgres"), "number[]");
    assert.equal(renderType(array(pg(name)), false, "postgres"), "(number[] | null)[]");
    assert.equal(renderParamType(array(pg(name)), "postgres"), "number[][]");
    assert.deepEqual(typeSchema(pg(name), "postgres"), { type: "array", items: { type: "number", format: "float" } });
    assert.deepEqual(typeSchema(pg(name), "postgres", false, true), { type: "string", description: `pg type ${name}` });
  }
  // A sparsevec stays its text.
  assert.equal(renderType(pg("sparsevec"), false, "postgres"), "string");
});

test("an hstore is an object of nullable text, as a column, a parameter and inside a document", () => {
  assert.equal(renderType(pg("hstore"), false, "postgres"), "Record<string, string | null>");
  assert.equal(renderParamType(pg("hstore"), "postgres"), "Record<string, string | null>");
  assert.equal(renderType(array(pg("hstore")), false, "postgres"), "(Record<string, string | null> | null)[]");
  assert.deepEqual(typeSchema(pg("hstore"), "postgres"), { type: "object", additionalProperties: { type: ["string", "null"] } });
  assert.equal(renderType(pg("hstore"), true, "postgres"), "Record<string, string | null>");
  assert.equal(renderType(doc({ kind: "array", element: { type: pg("hstore") } }), false, "postgres"), "(Record<string, string | null>)[]");
  assert.deepEqual(typeSchema(pg("hstore"), "postgres", false, true), { type: "object", additionalProperties: { type: ["string", "null"] } });
});

test("inside a document an oid is its text, a geometry its GeoJSON, and an extension's type with no cast to JSON its text", () => {
  // Postgres writes JSON numbers only for the integers, the floats and numeric: `{"o" : "7"}`.
  assert.equal(renderType(pg("oid"), false, "postgres"), "number");
  assert.equal(renderType(pg("oid"), true, "postgres"), "string");
  assert.deepEqual(typeSchema(pg("oid"), "postgres", false, true), { type: "string" });
  // PostGIS's cast to JSON writes GeoJSON: `{"type":"Point","coordinates":[1,2]}`.
  assert.equal(renderType(pg("geometry"), false, "postgres"), "string");
  assert.equal(renderType(pg("geometry"), true, "postgres"), "{ type: string; [key: string]: Json | undefined }");
  assert.deepEqual(typeSchema(pg("geometry"), "postgres", false, true), { type: "object", properties: { type: { type: "string" } }, required: ["type"], description: "a GeoJSON geometry" });
  // A pgvector vector, a geography, a cube or an isbn inside a document is the JSON string of its text: `{"v" : "[1,2,3]"}`.
  for (const name of ["vector", "halfvec", "sparsevec", "geography", "cube", "seg", "isbn", "semver", "ulid", "point"]) assert.equal(renderType(pg(name), true, "postgres"), "string", name);
});

test("a composite with no columns is an empty object", () => {
  assert.equal(renderType(record([], "public.empty"), true), "Record<never, never>");
  assert.equal(renderType(record([], "public.empty"), false), "Record<never, never>");
});

test("SQLite: an INTEGER is a number, exact or an error; a NUMERIC whichever the row stored; a column with no type unknown", () => {
  assert.equal(renderType(lite("integer"), false, "sqlite"), "number");
  assert.equal(renderType(lite("numeric"), false, "sqlite"), "number | string");
  assert.equal(renderType(lite("real"), false, "sqlite"), "number");
  assert.equal(renderType(lite("text"), false, "sqlite"), "string");
  assert.equal(renderType(lite("blob"), false, "sqlite"), "Uint8Array");
  assert.equal(renderType(lite("bool"), false, "sqlite"), "boolean");
  assert.equal(renderType({ kind: "boolean" }, false, "sqlite"), "boolean");
  assert.equal(renderType(lite("int8"), false, "sqlite"), "bigint");
  assert.equal(renderType({ kind: "unknown" }, false, "sqlite"), "unknown");
  // A column read as it is holds what its affinity stores, under the name its table declares.
  assert.equal(renderType({ ...lite("numeric"), label: "DECIMAL(10,2)" } as EngineType, false, "sqlite"), "number | string");
  assert.equal(renderType({ ...lite("numeric"), label: "bigint(20)" } as EngineType, false, "sqlite"), "bigint");
  assert.equal(renderType({ kind: "union", label: "VARCHAR(20)", members: [lite("text"), lite("blob")] }, false, "sqlite"), "string");
  // A number SQLite computes as an INTEGER or a REAL is a number, inside a document too.
  assert.equal(renderType(lite("number"), false, "sqlite"), "number");
  assert.equal(renderType(lite("number"), true, "sqlite"), "number");
  assert.equal(columnKind(lite("number"), "sqlite"), "int|float");
  assert.deepEqual(typeSchema(lite("number"), "sqlite"), { type: "number" });
  assert.deepEqual(typeSchema(lite("number"), "sqlite", false, true), { type: "number" });
  // The driver binds a number for an INTEGER, and the run time a truth value as 1 or 0.
  assert.equal(renderParamType(lite("integer"), "sqlite"), "number");
  assert.equal(renderParamType(lite("numeric"), "sqlite"), "number | string | boolean");
});

// Each column type beside the type the engine gives a parameter compared with that column, as
// the engine reported them on 2026-10-20 against Postgres 18.6, MySQL 8.4.11 and SQLite for the
// driver matrix's `typed` fixture (`select c from typed` and `select id from typed where c = :v`).
// MySQL describes every integer context as a BIGINT and a BIT one as BIT, so the engine types a
// parameter by the column it is compared with (engine/mysql `column_types`), and keeps the
// prepare's (`int4`, `bool|bytea`) for an expression; SQLite types a parameter by the column's
// affinity, a BOOLEAN column's being NUMERIC.
const composite: EngineType = record([field("name", pg("text"), true)], "public.user_in");
const mood = enumOf("public.mood", ["ok", "sad"]);
const ROUND_TRIPS: Record<"postgres" | "mysql" | "sqlite", [column: EngineType, param: EngineType][]> = {
  postgres: [
    ...["int2", "int4", "int8", "float4", "float8", "numeric", "bool", "text", "uuid", "interval", "time", "money", "date", "timestamp", "timestamptz", "bytea"].map((n): [EngineType, EngineType] => [pg(n), pg(n)]),
    [pg("jsonb"), pg("jsonb")],
    [mood, mood],
    ...["int4", "int8", "text", "bool", "timestamptz", "date", "bytea", "numeric"].map((n): [EngineType, EngineType] => [array(pg(n)), array(pg(n))]),
    [array(mood), array(mood)],
    [array(composite), array(composite)],
    [array(pg("record")), array(pg("record"))],
    // An array the statement builds from values that may be NULL (`array_agg` over a left join)
    // goes back into a structured member alone: a top-level array parameter takes no NULL
    // element, tested below.
  ],
  mysql: [
    [my("bigint"), my("bigint")],
    [my("int"), { kind: "integer", bits: 32 }],
    [my("smallint"), { kind: "integer", bits: 32 }],
    [my("tinyint(1)"), my("tinyint(1)")],
    [my("bit", 1), oneOf({ kind: "boolean" }, my("bit"))],
    [my("bit", 8), oneOf({ kind: "boolean" }, my("bit"))],
    [my("bit", 8), my("bit", 8)],
    ...["decimal", "double", "varchar", "date", "datetime", "timestamp", "time"].map((n): [EngineType, EngineType] => [my(n), my(n)]),
    [my("json"), my("json")],
    // A SET column's parameter is typed by the column, as the engine's catalog reads it (engine/mysql `mysql_params.rs`).
    [my("set"), my("set")],
  ],
  sqlite: [
    ...["integer", "int8", "real", "text", "blob", "numeric"].map((n): [EngineType, EngineType] => [lite(n), lite(n)]),
    [lite("bool"), lite("numeric")],
    [{ kind: "unknown" }, { kind: "unknown" }],
  ],
};

/** An array type's element type, `(a | b)[]` as `a | b`; undefined for any other type. */
const element = (ts: string) => (ts.endsWith("[]") ? ts.slice(0, -2).replace(/^\((.*)\)$/, "$1") : undefined);

/**
 * Whether every value of the column's type is one the parameter's type takes, member by member of
 * the union, element by element of an array. An array's NULL elements are the one exception: a
 * column's may hold them, and a parameter's elements are strict, so an array goes back in without
 * them.
 */
function admits(column: EngineType, param: EngineType, dialect: "postgres" | "mysql" | "sqlite"): boolean {
  const returned = renderType(column, false, dialect);
  const taken = renderParamType(param, dialect);
  const members = (ts: string) => ts.split(" | ");
  const [returnedElement, takenElement] = [element(returned), element(taken)];
  if (returnedElement !== undefined && takenElement !== undefined) return members(returnedElement).filter((m) => m !== "null").every((m) => members(takenElement).includes(m) || members(takenElement).includes("unknown"));
  // A JSON parameter with no schema takes whatever `JSON.stringify` writes, every `Json` among it.
  if (taken === "JsonInput") return returned === "Json";
  return members(taken).includes("unknown") || members(returned).every((member) => members(taken).includes(member));
}

test("a parameter takes every value run returns for the column it is compared with, on every dialect: the round-trip rule", () => {
  for (const [dialect, pairs] of Object.entries(ROUND_TRIPS) as ["postgres" | "mysql" | "sqlite", [EngineType, EngineType][]][]) {
    for (const [column, param] of pairs) {
      assert.ok(admits(column, param, dialect), `${dialect}: a column of ${JSON.stringify(column)} returns ${renderType(column, false, dialect)}, which a parameter of ${JSON.stringify(param)}, typed ${renderParamType(param, dialect)}, does not take`);
    }
  }
});

test("parameters take what every driver binds, whatever Reylo returns", () => {
  assert.equal(renderParamType(pg("numeric"), "postgres"), "string");
  assert.equal(renderParamType(pg("int8"), "postgres"), "number | bigint | string");
  assert.equal(renderParamType(lite("blob"), "sqlite"), "Uint8Array");
  assert.equal(renderParamType(my("varbinary"), "mysql"), "Uint8Array");
  // A JSON value, built by the statement or read from a column, is parsed.
  assert.equal(renderType(doc(record([field("id", my("int"))])), false, "mysql"), "{ id: number }");
  // Inside a JSON value the database wrote the bytes and the number already. A non-finite decimal
  // or float is the text Postgres writes inside a document (`{"n": "NaN"}`), which the run time
  // gives as the number it is; MySQL stores none.
  assert.equal(renderType(doc(record([field("b", pg("bytea")), field("n", pg("numeric"))])), false, "postgres"), "{ b: string; n: number }");
  assert.equal(renderType(doc(record([field("b", my("varbinary")), field("n", my("decimal"))])), false, "mysql"), "{ b: string; n: number }");
  // A union's member keeps the text, which the run time leaves as it is.
  assert.equal(renderType(doc(record([field("u", oneOf(pg("float8"), pg("int4")))])), false, "postgres"), '{ u: number | "NaN" | "Infinity" | "-Infinity" }');
});

test("the run time converts each float or decimal inside Postgres JSON that may be NaN or an infinity, and only those", () => {
  const sized = { kind: "decimal", precision: 10, min_scale: 2, max_scale: 2, nan: true, label: "numeric" } as const;
  const finite = { kind: "float", exponent_bits: 11, mantissa_bits: 52, label: "float8" } as const;
  const row = record([field("id", pg("int4")), field("n", pg("numeric")), field("p", sized), field("f", finite), field("at", pg("point")), field("xs", array(pg("float8")))]);
  assert.deepEqual(jsonNumbers(doc(row)), { keys: [["n", true], ["p", true], ["xs", { items: true }]] });
  // An array a statement aggregates, of JSON, and a Postgres array of JSON values.
  assert.deepEqual(jsonNumbers(doc({ kind: "array", element: { type: row } })), { items: { keys: [["n", true], ["p", true], ["xs", { items: true }]] } });
  assert.deepEqual(jsonNumbers(array(doc(record([field("n", pg("float4"))])))), { items: { keys: [["n", true]] } });
  // A Postgres array inside a document is held to one level, of floats or of anything whose JSON is
  // no array, and to none under allow_nested_arrays; one of JSON values may hold arrays at any depth.
  const tags = record([field("xs", array(pg("float8"), true, "_float8")), field("ts", array(pg("text"), true, "_text")), field("js", array(pg("jsonb"), true, "_jsonb"))]);
  assert.deepEqual(jsonNumbers(doc(tags)), { keys: [["xs", { items: true, flat: true }], ["ts", { items: false, flat: true }]] });
  const nested = analysisWithNestedArrays({ columns: [{ type: doc(tags) }] }).columns[0]!.type;
  assert.deepEqual(jsonNumbers(nested), { keys: [["xs", { items: true, deep: true }]] });
  assert.deepEqual(numbersInJson([1, 2], { items: false, flat: true }, "doc"), [1, 2]);
  assert.throws(() => numbersInJson({ xs: [[1, 2]] }, { keys: [["xs", { items: true, flat: true }]] }, "doc"), /Column "doc" holds, inside its JSON, a Postgres array of more than one dimension/);
  // Outside JSON a float is a number already, and a row value is its text; a number a CHECK keeps
  // clear, a geometric type written as text and a union need nothing.
  assert.equal(jsonNumbers(pg("float8")), undefined);
  assert.equal(jsonNumbers(array(pg("float8"))), undefined);
  assert.equal(jsonNumbers(record([field("n", pg("numeric"))])), undefined);
  assert.equal(jsonNumbers(doc(record([field("f", finite), field("at", pg("point")), field("u", oneOf(pg("float8"), pg("int4")))]))), undefined);
  // Two keys of one name keep the last.
  assert.deepEqual(jsonNumbers(doc(record([field("x", pg("float8")), field("x", pg("int4"))]))), undefined);
});

test("the run time refuses an integer past 2^53 - 1 where a 64-bit integer stands inside JSON, on every dialect but ClickHouse", () => {
  // `json_build_object('id', id, 'n', n, 'big', big)`, and its SQLite and MySQL forms.
  assert.deepEqual(jsonNumbers(doc(record([field("id", pg("int4")), field("big", pg("int8")), field("o", pg("oid"))]))), { keys: [["big", "integer"]] });
  assert.deepEqual(jsonNumbers(doc({ kind: "array", element: { type: record([field("amount", lite("int8")), field("n", lite("integer")), field("lead", lite("bool"))]) } }), false, "sqlite"), { items: { keys: [["amount", "integer"], ["n", "integer"]] } });
  assert.deepEqual(jsonNumbers(doc(record([field("id", my("int")), field("big", my("bigint")), field("u", { kind: "integer", bits: 64, unsigned: true })])), false, "mysql"), { keys: [["big", "integer"], ["u", "unsigned"]] });
  assert.equal(jsonNumbers(doc(record([field("big", { kind: "integer", bits: 64 })])), false, "clickhouse"), undefined);
  // Outside JSON a 64-bit integer is a BigInt already, and on SQLite and MySQL a float is a number inside JSON too.
  assert.equal(jsonNumbers(pg("int8")), undefined);
  assert.equal(jsonNumbers(lite("int8"), false, "sqlite"), undefined);
  assert.equal(jsonNumbers(doc(record([field("f", lite("real"))])), false, "sqlite"), undefined);
  const rounds = { items: { keys: [["amount", "integer"]] } } as const;
  assert.deepEqual(numbersInJson([{ amount: 9007199254740991 }, { amount: -9007199254740991 }, { amount: null }], rounds, "rounds"), [{ amount: 9007199254740991 }, { amount: -9007199254740991 }, { amount: null }]);
  assert.throws(() => numbersInJson([{ amount: 1 }, { amount: 9007199254740993 }], rounds, "rounds"), /Column "rounds" holds, inside its JSON at \/1\/amount, the integer 9007199254740992, which is past 2\^53 - 1, where JSON\.parse rounds a number/);
  assert.throws(() => numbersInJson({ "a/b": -9007199254740992 }, { keys: [["a/b", "integer"]] }, "doc"), /at \/a~1b, the integer -9007199254740992, which is below -\(2\^53 - 1\)/);
  // A float past what a 64-bit integer holds, as a non-STRICT SQLite INTEGER column may store, is one JSON.parse reads exactly.
  assert.deepEqual(numbersInJson({ n: 1e19 }, { keys: [["n", "integer"]] }, "doc"), { n: 1e19 });
  assert.throws(() => numbersInJson({ n: -(2 ** 63) }, { keys: [["n", "integer"]] }, "doc"), /the integer -9223372036854775808/);
  assert.throws(() => numbersInJson({ n: 1e19 }, { keys: [["n", "unsigned"]] }, "doc"), /the integer 10000000000000000000/);
  // An integer of one width or another, and an enum of integers; a SQLite NUMERIC may be a float.
  assert.deepEqual(jsonNumbers(doc(record([field("n", oneOf(my("bigint"), my("int"))), field("e", { kind: "enum", type: pg("int8"), values: [{ value: "1" }] } as EngineType), field("x", lite("numeric"))])), false, "mysql"), { keys: [["n", "integer"], ["e", "integer"]] });
  // The schema the engine writes for `json_build_array(id, amount)` and SQLite's `json_object(k, amount)`.
  const tuple = { kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "array", prefixItems: [{ type: "integer", format: "int32" }, { type: "integer", format: "int64" }], items: false, minItems: 2 } } as EngineType;
  assert.deepEqual(jsonNumbers(tuple), { items: false, elements: [null, "integer"] });
  const keyed = { kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "object", additionalProperties: { anyOf: [{ type: "integer", format: "int64" }, { type: "null" }] } } } as EngineType;
  assert.deepEqual(jsonNumbers(keyed, false, "sqlite"), { keys: [], others: "integer" });
  assert.deepEqual(jsonNumbers(doc(record([field("xs", tuple)]))), { keys: [["xs", { items: false, elements: [null, "integer"] }]] });
  // A SQLite integer column that may hold a REAL: a whole number from 2^53 to 2^63 may be a rounded integer, as its column reads; a float beside an integer elsewhere is exact.
  const integerOrReal = { kind: "union", label: "INTEGER", members: [{ kind: "integer", bits: 64 }, { kind: "float", exponent_bits: 11, mantissa_bits: 52, infinity: true }] } as EngineType;
  assert.deepEqual(jsonNumbers(doc(record([field("n", integerOrReal)])), false, "sqlite"), { keys: [["n", "integer"]] });
  assert.equal(jsonNumbers(doc(record([field("n", integerOrReal)])), false, "postgres"), undefined);
  // So does such a value in an array or an object the engine describes by a schema, `json_array(id, amount)` and `json_group_object(k, amount)`.
  const integerOrRealSchema = { anyOf: [{ type: "integer", format: "int64" }, { type: "number", format: "double" }, { type: "null" }] };
  const builtArray = { kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "array", prefixItems: [{ type: "integer", format: "int64" }, integerOrRealSchema], items: false } } as EngineType;
  assert.deepEqual(jsonNumbers(builtArray, false, "sqlite"), { items: false, elements: ["integer", "integer"] });
  assert.deepEqual(jsonNumbers({ kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "object", additionalProperties: integerOrRealSchema } } as EngineType, false, "sqlite"), { keys: [], others: "integer" });
  assert.deepEqual(jsonNumbers(builtArray, false, "postgres"), { items: false, elements: ["integer", null] });
  // `JSON_ARRAY(big, n)` with a nullable int beside the bigint: each place an integer of one width or another.
  assert.deepEqual(jsonNumbers({ kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "array", items: { anyOf: [{ type: "integer", format: "int64" }, { type: "integer", format: "int32" }] } } } as EngineType), { items: "integer" });
  // An integer or a float at a place holds numbers a double holds exactly.
  assert.equal(jsonNumbers({ kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "array", prefixItems: [{ anyOf: [{ type: "integer", format: "int64" }, { type: "number" }] }], items: false } } as EngineType), undefined);
  assert.equal(jsonNumbers({ kind: "json", schema: { "$schema": "https://json-schema.org/draft/2020-12/schema", type: "array", items: { type: "integer" } } } as EngineType), undefined);
  assert.throws(() => numbersInJson([1, 9007199254740993], { items: false, elements: [null, "integer"] }, "a"), /at \/1, the integer 9007199254740992/);
  assert.deepEqual(numbersInJson([9007199254740993, 1], { items: false, elements: [null, "integer"] }, "a"), [9007199254740992, 1]);
  assert.throws(() => numbersInJson({ ok: 1, big: 9007199254740993 }, { keys: [], others: "integer" }, "b"), /at \/big, the integer/);
});

test("two columns or keys of one name keep the last, as a row object and JSON.parse do", () => {
  assert.equal(renderType(doc(record([field("x", pg("text")), field("x", pg("int4"), true)])), false, "postgres"), "{ x: number | null }");
  // A row whose columns were not read (a subquery's) is an object of them inside a document.
  assert.equal(renderType(doc({ kind: "array", element: { type: pg("record") } }), false, "postgres"), "Record<string, Json>[]");
});

test("every date and time parameter takes a string: a timestamptz one the text that names its zone, which run returns, and no Date", () => {
  assert.equal(renderParamType(pg("timestamptz"), "postgres"), "string");
  assert.equal(renderParamType(array(pg("timestamptz")), "postgres"), "string[]");
  for (const name of ["timestamp", "date"]) {
    assert.equal(renderParamType(pg(name), "postgres"), "string");
    assert.equal(renderParamType(array(pg(name)), "postgres"), "string[]");
  }
  for (const name of ["datetime", "timestamp", "date"]) assert.equal(renderParamType(my(name), "mysql"), "string");
  // What MySQL answers by what is bound for a placeholder compared with a DATE and a DATETIME.
  assert.equal(renderParamType(oneOf(my("date"), my("datetime")), "mysql"), "string");
});

test("a returned array's elements may be NULL unless the statement shows they are not; a parameter's are strict", () => {
  // Postgres lets any array hold NULL elements, whatever a column's NOT NULL says: a table's array
  // column is `(T | null)[]`, and so is `array_agg(u.email)` over a LEFT JOIN, `{NULL}` for a team
  // with no users. An array the statement builds from values it shows are not NULL is `T[]`.
  const texts = (nullable = true): EngineType => array(pg("text"), nullable);
  assert.equal(renderType(texts()), "(string | null)[]");
  assert.equal(renderType(texts(false)), "string[]");
  assert.equal(renderType(texts(), true), "(string | null)[]");
  assert.equal(renderType(array(enumOf("public.mood", ["ok", "sad"]))), `("ok" | "sad" | null)[]`);
  // A top-level array parameter's elements take no NULL, as the parameter itself takes NULL only
  // when cast to `nullable(…)` or written bare to a nullable column: a NULL in `= any(:ids)`
  // matches nothing. An array inside a
  // structured parameter's member keeps NULL elements, so a built array goes back into a row.
  assert.equal(renderParamType(texts()), "string[]");
  assert.equal(renderParamType(array(pg("int8"))), "(number | bigint | string)[]");
  assert.equal(renderParamType(array(pg("int4"))), "number[]");
  const member = record([field("tags", texts())]);
  const members = [{ written: "text[]" }];
  assert.equal(renderParamType(member, "postgres", false, members), "{ tags: (string | null)[] }");
  // The JSON Schemas say the same.
  assert.deepEqual(typeSchema(texts(), "postgres"), { type: "array", items: { type: ["string", "null"] } });
  assert.deepEqual(typeSchema(texts(false), "postgres"), { type: "array", items: { type: "string" } });
  assert.deepEqual(typeSchema(texts(), "postgres", true), { type: "array", items: { type: "string" } });
  assert.deepEqual(typeSchema(member, "postgres", true, false, false, members), { type: "object", properties: { tags: { type: "array", items: { type: ["string", "null"] } } }, required: ["tags"], additionalProperties: false });
});

test("a JSON value nothing constrains is any JSON value, Json, and so is a value nothing types inside JSON the statement builds; its parameter takes whatever JSON.stringify writes", () => {
  for (const dialect of ["postgres", "mysql", "sqlite"] as const) {
    assert.equal(renderType({ kind: "json" }, false, dialect), "Json");
    assert.equal(renderParamType({ kind: "json" }, dialect), "JsonInput");
  }
  // Json admits null, which a NOT NULL jsonb column can hold, so a nullable one reads the same; so does JsonInput.
  assert.equal(orNull("Json"), "Json");
  assert.equal(orNull("JsonInput"), "JsonInput");
  assert.equal(orNull("string"), "string | null");
  assert.equal(renderType(array(pg("jsonb"))), "Json[]");
  assert.equal(renderType(doc({ kind: "array", element: { type: { kind: "unknown" }, nullable: true } })), "Json[]");
  assert.equal(renderType(doc(record([field("k", { kind: "unknown" }, true), field("n", pg("jsonb")), field("s", pg("text"), true)]))), "{ k: Json; n: Json; s: string | null }");
  assert.equal(renderType(pg("mystery"), true, "postgres"), "Json");
  // Outside JSON, a value nothing types may be any value: SQLite's json_extract, a column with no declared type.
  assert.equal(renderType({ kind: "unknown" }, false, "sqlite"), "unknown");
});

test("a JSON column a CHECK holds to a JSON Schema is the type of the values the schema admits", () => {
  const json = checked;
  const open = { type: "object", properties: { name: { type: "string" }, age: { type: ["integer", "null"] } }, required: ["name"], additionalProperties: true };
  // Other keys that may hold anything need no index signature beside the named ones, so a misspelled key is an error.
  assert.equal(renderType(json(open)), "{ name: string; age?: number | null }");
  assert.equal(renderType(json({ ...open, additionalProperties: false })), "{ name: string; age?: number | null }");
  assert.equal(renderType(json({ ...open, additionalProperties: { type: "boolean" } })), "{ name: string; age?: number | null; [key: string]: Json | undefined }");
  assert.equal(renderType(json({ type: "object", additionalProperties: true })), "{ [key: string]: Json }");
  assert.equal(renderType(json({ type: "object", additionalProperties: { type: "string" } })), "{ [key: string]: string }");
  assert.equal(renderType(json({ type: "object", additionalProperties: false })), "Record<never, never>");
  assert.equal(renderType(json({ type: ["object", "null"] })), "Record<string, Json> | null");
  // A discriminated union, a literal object, a tuple with an optional element and a rest.
  const tagged = { anyOf: [
    { type: "object", properties: { kind: { enum: ["a"] }, x: { type: "number" } }, required: ["kind", "x"], additionalProperties: false },
    { type: "object", properties: { kind: { enum: ["b"] } }, required: ["kind"], additionalProperties: false },
  ] };
  assert.equal(renderType(json(tagged)), `{ kind: "a"; x: number } | { kind: "b" }`);
  // An open object among a union's objects keeps its signature: `"x" in v` would otherwise drop
  // the `b` member, which may hold an `x` of any type, and narrow `v.x` to a number.
  const openTagged = { anyOf: tagged.anyOf.map((s) => ({ ...s, additionalProperties: true })) };
  assert.equal(renderType(json(openTagged)), `{ kind: "a"; x: number; [key: string]: Json | undefined } | { kind: "b"; [key: string]: Json | undefined }`);
  assert.equal(renderParamType(json(openTagged), "postgres"), `{ kind: "a"; x: number; [key: string]: Json | undefined } | { kind: "b"; [key: string]: Json | undefined }`);
  assert.equal(renderType(json({ anyOf: [{ anyOf: [open, { type: "string" }] }, { enum: [{ name: "x" }] }] })), `{ name: string; age?: number | null; [key: string]: Json | undefined } | string | { name: "x" }`);
  // One object in a union, beside other types, has no other object to narrow to.
  assert.equal(renderType(json({ anyOf: [open, { type: "string" }] })), "{ name: string; age?: number | null } | string");
  assert.equal(renderType(json({ ...open, type: ["object", "null"] })), "{ name: string; age?: number | null } | null");
  // A property's own schema is no member of the union around its object.
  assert.equal(renderType(json({ anyOf: [{ type: "object", properties: { inner: open }, required: ["inner"], additionalProperties: false }, { type: "object", properties: { other: { type: "string" } }, additionalProperties: false }] })), "{ inner: { name: string; age?: number | null } } | { other?: string }");
  assert.equal(renderType(json({ enum: [{ a: 1, "b-c": [true, null] }, "x", 2, null] })), `{ a: 1; "b-c": [true, null] } | "x" | 2 | null`);
  assert.equal(renderType(json({ type: "array", prefixItems: [{ type: "string" }, { type: ["number", "null"] }], items: { type: "boolean" }, minItems: 1 })), "[string, (number | null)?, ...boolean[]]");
  assert.equal(renderType(json({ type: "array", prefixItems: [{ type: "string" }], items: false, minItems: 1 })), "[string]");
  assert.equal(renderType(json({ type: "array", items: { type: ["string", "integer"] } })), "(string | number)[]");
  assert.equal(renderType(json({ type: "array", items: false })), "[]");
  assert.equal(renderType(json(false)), "never");
  assert.equal(renderType(json({})), "Json");
  assert.equal(renderType(json({ type: "array" })), "Json[]");
  // Reylo parses JSON on every driver; inside a document the schema's type stands, and a parameter's
  // integer takes a BigInt too, which `.run()` writes as its digits, so one past 2^53 - 1 stays exact.
  assert.equal(renderType(json(open), true, "postgres"), "{ name: string; age?: number | null }");
  assert.equal(renderParamType(json(open), "postgres"), "{ name: string; age?: number | bigint | null }");
  assert.equal(renderParamType(json({ type: "array", items: { type: ["number", "integer"], minimum: 0 } }), "sqlite"), "(number | bigint)[]");
  assert.equal(renderParamType(json({ type: "array", prefixItems: [{ type: "integer" }], items: { type: "number" } }), "mysql"), "[(number | bigint)?, ...number[]]");
  // Beside a field that takes a BigInt, an index signature takes any value, which the run time checks against the schema.
  assert.equal(renderParamType(json({ type: "object", properties: { id: { type: "integer" } }, required: ["id"], additionalProperties: { type: "string" } }), "postgres"), "{ id: number | bigint; [key: string]: unknown }");
  assert.equal(renderType({ kind: "json" }), "Json");
});

test("a tagged union is { tag, value } as a value, and any JSON inside a document", () => {
  const choice = { kind: "tagged_union", alternatives: [{ tag: "num", type: { kind: "integer", bits: 64 }, nullable: true }, { tag: "str", type: { kind: "text" }, nullable: true }], label: "union" } as unknown as EngineType;
  assert.equal(renderType(choice, false), `{ tag: "num"; value: bigint | null } | { tag: "str"; value: string | null }`);
  // A 64-bit integer is a bigint as a value, never inside parsed JSON.
  assert.equal(renderType(choice, true), "Json");
  assert.deepEqual(typeSchema(choice, "postgres", false, true), {});
});
