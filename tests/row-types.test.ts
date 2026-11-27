import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Session } from "../src/analyze.ts";
import { sqlModule } from "../src/loader.ts";
import { clickhouseDatabase, clickhouseLive, closeSessions, DRIVERS, INDEX, mysqlDatabase, mysqlLive, pgLive, postgresDatabase, ROOT } from "./helpers.ts";

// Every type of each database, at the edges of what it holds, as `.run()` returns it and as the
// server writes it inside each kind of JSON document it builds, held to the type the content mapper
// declares for the result: each result written as a TypeScript literal of its values, a BigInt, bytes
// and a Set included, and type-checked against that type. A built array of one value
// (`json_build_array`, `json_array`) is typed by the engine's own writers, and an object or an
// aggregate by the loader's, so both meet the same values.

/** A value as a TypeScript literal of what it is: a BigInt as one, bytes and a Set as the objects they are, a key as a computed one, which `__proto__` is too. */
function literal(v: unknown): string {
  if (v === null) return "null";
  if (typeof v === "bigint") return `${v}n`;
  if (typeof v === "number") return Object.is(v, -0) ? "-0" : String(v);
  if (typeof v === "string" || typeof v === "boolean") return JSON.stringify(v);
  if (v instanceof Uint8Array) return `new Uint8Array([${[...v].join(",")}])`;
  if (v instanceof Set) return `new Set([${[...v].map(literal).join(",")}])`;
  if (Array.isArray(v)) return `[${v.map(literal).join(",")}]`;
  if (typeof v === "object") return `{ ${Object.entries(v as Record<string, unknown>).map(([k, x]) => `[${JSON.stringify(k)}]: ${literal(x)}`).join(", ")} }`;
  throw new Error(`no literal for ${String(v)}`);
}

/** A project at a new directory on `url`, with `drivers` from lab/drivers installed, whose `main.ts` the type check reads. */
function project(url: string, drivers: string[] = []): string {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "reylo-row-types-")));
  writeFileSync(join(root, "package.json"), `{ "type": "module" }`);
  writeFileSync(join(root, ".env"), `DATABASE_URL=${url}\n`);
  mkdirSync(join(root, "node_modules", "@clickhouse"), { recursive: true });
  symlinkSync(ROOT, join(root, "node_modules", "reylo"));
  for (const d of drivers) symlinkSync(join(DRIVERS, d), join(root, "node_modules", d));
  const compilerOptions = { target: "esnext", module: "nodenext", lib: ["esnext"], types: [], strict: true, noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true };
  writeFileSync(join(root, "tsconfig.json"), JSON.stringify({ compilerOptions, contentMappers: [{ package: "reylo" }], include: ["main.ts"] }));
  return root;
}

type Built = { run(client: unknown): Promise<unknown> | unknown };

/** The module a build step writes for `sql` in `root`, imported. */
async function built(root: string, name: string, sql: string): Promise<Built> {
  const file = join(root, `${name}.sql`);
  writeFileSync(file, sql);
  try {
    const { code } = await sqlModule(file, sql);
    const out = join(root, `${name}.mjs`);
    writeFileSync(out, code.replace(`from "reylo"`, `from ${JSON.stringify(INDEX)}`));
    return ((await import(pathToFileURL(out).href)) as { default: Built }).default;
  } finally {
    for (const session of Session.opened()) await session.close();
  }
}

/** A type, the column it declares (`c`, with a CHECK after the type where it has one), and the values its rows hold, as SQL literals. */
type Case = readonly [name: string, column: string, values: readonly string[]];

/**
 * Each case's statements, built and run, and everything the module says of the rows held to them
 * (above). The problems, one line each, so that one run names every type a writer gets wrong.
 */
async function check(root: string, cases: readonly Case[], statements: (table: string, c: Case) => readonly string[], run: (q: Built) => Promise<unknown>): Promise<string[]> {
  const problems: string[] = [];
  const typed: { file: string; about: string; rows: unknown }[] = [];
  for (const [i, c] of cases.entries()) {
    for (const [j, sql] of statements(`t${i}`, c).entries()) {
      const about = `${c[0]}: ${sql}`;
      let q: Built;
      try {
        q = await built(root, `q${i}_${j}`, `${sql}\n`);
      } catch (e) {
        problems.push(`${about}: the build throws ${(e as Error).message.split("\n")[0]}`);
        continue;
      }
      try {
        typed.push({ file: `q${i}_${j}`, about, rows: await run(q) });
      } catch (e) {
        problems.push(`${about}: the run throws ${(e as Error).message.split("\n")[0]}`);
      }
    }
  }
  // Each statement's result as a value of the type its module declares for it.
  writeFileSync(join(root, "main.ts"), `import type { Materialize } from "reylo";\n${typed.map(({ file, rows }) => `import type ${file} from "./${file}.sql";\nexport const ${file}_rows: Materialize<(typeof ${file})["~reylo"]["element"], (typeof ${file})["~reylo"]["cardinality"]> = ${literal(rows)};\n`).join("")}`);
  const tsc = spawnSync(process.execPath, [join(ROOT, "node_modules", "typescript", "bin", "tsc"), "-p", "tsconfig.json", "--runExternalCode", "--pretty", "false"], { cwd: root, encoding: "utf8" });
  for (const line of (tsc.stdout + tsc.stderr).split("\n").filter(Boolean)) {
    const at = /^main\.ts\((\d+),/.exec(line);
    const entry = at ? typed[Math.floor((Number(at[1]) - 2) / 2)] : undefined;
    problems.push(entry ? `${entry.about}: the declared type refuses ${literal(entry.rows)}: ${line}` : `tsc: ${line}`);
  }
  return problems;
}

/** A table per case, `id` and `c`, a row per value and one of NULL. */
function tables(cases: readonly Case[], id = "int primary key"): string {
  return cases.map(([, column, values], i) => `create table t${i} (id ${id}, c ${column});\ninsert into t${i} values ${[...values, "null"].map((v, n) => `(${n + 1}, ${v})`).join(", ")};`).join("\n");
}

const POSTGRES: readonly Case[] = [
  ["bool", "bool", ["true", "false"]],
  ["int2", "int2", ["-32768", "32767"]],
  ["int4", "int4", ["-2147483648", "2147483647"]],
  ["int8", "int8", ["-9223372036854775808", "9223372036854775807"]],
  // A divisor a CHECK states, of values past 2^53, which a double reads rounded.
  ["int8 multiple of 3", "int8 check (c % 3 = 0)", ["9007199254740993", "-9223372036854775806", "3"]],
  ["int4 multiple of 3", "int4 check (c % 3 = 0) check (c between -9 and 9)", ["-9", "9"]],
  ["oid", "oid", ["0", "4294967295"]],
  ["float4", "float4", ["'-0'", "3.4028235e38", "1e-45"]],
  ["float8", "float8", ["'-0'", "1.7976931348623157e308", "5e-324"]],
  ["numeric", "numeric", ["-0.000000001", "1e30", "0.29999999999999999"]],
  ["numeric(10,2)", "numeric(10,2)", ["-99999999.99", "0.01"]],
  // A bound a double rounds, and one it holds.
  ["numeric at least 0.29999999999999999", "numeric check (c >= 0.29999999999999999)", ["0.29999999999999999", "1"]],
  ["numeric(4,2) multiple of 0.05", "numeric(4,2) check (c % 0.05 = 0) check (c > 0.1)", ["0.15", "99.95"]],
  // A negative scale, whose values reach past what a double holds exactly.
  ["numeric(15,-2) multiple of 24", "numeric(15,-2) check (c % 24 = 0)", ["90071992547409000", "2400"]],
  ["money", "money", ["-92233720368547758.08", "92233720368547758.07"]],
  ["text", "text", ["''", "'hi'"]],
  ["varchar(3)", "varchar(3)", ["'abc'"]],
  ["char(3)", "char(3)", ["'a'"]],
  ["name", "name", ["'pg_class'"]],
  ["citext", "citext", ["'Hi'"]],
  ["bytea", "bytea", ["'\\x'", "'\\x00ff'"]],
  ["uuid", "uuid", ["'00000000-0000-0000-0000-000000000000'", "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'"]],
  ["date", "date", ["'4714-11-24 BC'", "'2026-01-01'", "'5874897-12-31'", "'infinity'", "'-infinity'"]],
  ["timestamp", "timestamp", ["'4714-11-24 00:00:00 BC'", "'2026-01-01 10:00:00.5'", "'294276-12-31 23:59:59.999999'", "'infinity'"]],
  ["timestamptz", "timestamptz", ["'4714-11-24 00:00:00+00 BC'", "'2026-01-01 10:00:00.5+02'", "'294276-12-31 23:59:59.999999+00'", "'-infinity'"]],
  ["time", "time", ["'00:00:00'", "'24:00:00'"]],
  ["timetz", "timetz", ["'00:00:00+15:59'", "'24:00:00-15:59'"]],
  ["interval", "interval", ["'-178000000 years'", "'1 mon -1 day'", "'infinity'"]],
  ["json", "json", [`'{"a":1}'`, "'null'", `'[1,"x"]'`]],
  ["jsonb", "jsonb", [`'{"a":1}'`, "'null'", `'[1,"x"]'`]],
  ["xml", "xml", ["'<a/>'"]],
  ["inet", "inet", ["'10.0.0.1'", "'::1/128'"]],
  ["cidr", "cidr", ["'10.0.0.0/8'"]],
  ["macaddr", "macaddr", ["'08:00:2b:01:02:03'"]],
  ["macaddr8", "macaddr8", ["'08:00:2b:01:02:03:04:05'"]],
  ["bit(3)", "bit(3)", ["B'101'"]],
  ["varbit", "varbit", ["B''", "B'1'"]],
  ["point", "point", ["'(1.5,-2)'"]],
  ["line", "line", ["'{1,-1,0}'"]],
  ["lseg", "lseg", ["'[(0,0),(1,1)]'"]],
  ["box", "box", ["'(3,4),(1,2)'"]],
  ["path", "path", ["'[(0,0),(1,1)]'", "'((0,0),(1,1))'"]],
  ["polygon", "polygon", ["'((0,0),(1,1),(2,0))'"]],
  ["circle", "circle", ["'<(1,2),3>'"]],
  ["int4range", "int4range", ["'[1,5)'", "'empty'", "'(,)'"]],
  ["numrange", "numrange", ["'[0.5,1]'"]],
  ["tstzrange", "tstzrange", ["'[2026-01-01,infinity)'"]],
  ["daterange", "daterange", ["'[2026-01-01,2026-02-01)'"]],
  ["int4multirange", "int4multirange", ["'{[1,2),[5,6)}'", "'{}'"]],
  ["int4[]", "int4[]", ["'{1,NULL}'", "'{}'"]],
  ["int8[]", "int8[]", ["'{9223372036854775807}'"]],
  ["text[]", "text[]", [`'{a,"b c"}'`]],
  ["numeric[]", "numeric[]", ["'{1.5,-0.000000001}'"]],
  ["bytea[]", "bytea[]", ["'{\"\\\\x00ff\"}'"]],
  ["tsvector", "tsvector", ["'a b'"]],
  ["tsquery", "tsquery", ["'a & b'"]],
  ["pg_lsn", "pg_lsn", ["'0/0'"]],
  ["regclass", "regclass", ["'pg_class'"]],
  ["hstore", "hstore", ["'a=>1, b=>NULL'"]],
  ["ltree", "ltree", ["'a.b'"]],
  ["vector", "vector(3)", ["'[1,2,3]'"]],
  ["halfvec", "halfvec(2)", ["'[1,2]'"]],
  ["sparsevec", "sparsevec(3)", ["'{1:1}/3'"]],
  ["enum", "mood", ["'ok'"]],
  ["composite", "pair", ["row(1, 'a')", "row(null, null)"]],
  ["domain", "posint", ["1"]],
  // Postgres writes a field and an element inside a document as it writes the value there.
  ["composite of text forms", "shape", ["row('(1,2)', '[1,3)', 5)"]],
  ["point[]", "point[]", ["'{\"(1,2)\",NULL}'"]],
  ["domain over money", "cash", ["5"]],
  // A composite of the user's named as an extension's type or a built-in one is an object of its fields, whose name is qualified.
  ["composite named like an extension's type", "semver", ["row(1, 2)"]],
  ["composite named like a built-in type", "public.point", ["row(1)"]],
];

/** The extension each case's type comes from, whose cases a server without it skips, saying so: a Postgres installed on Windows has no pgvector. */
const EXTENSIONS: Readonly<Record<string, string>> = { citext: "citext", hstore: "hstore", ltree: "ltree", vector: "vector", halfvec: "vector", sparsevec: "vector" };

test("Postgres: every type's values are of the type the module declares, as a column and inside each document Postgres builds", { skip: pgLive }, async (t) => {
  const own = await postgresDatabase("reylo_output_schema");
  try {
    const available = new Set((await own.sql`select name from pg_available_extensions`).map((r: { name: string }) => r.name));
    for (const missing of new Set(Object.values(EXTENSIONS).filter((e) => !available.has(e)))) await t.test(`${missing}'s types`, { skip: `the test server has no ${missing}` }, () => {});
    const cases = POSTGRES.filter(([name]) => EXTENSIONS[name] === undefined || available.has(EXTENSIONS[name]));
    const extensions = [...new Set(Object.values(EXTENSIONS))].filter((e) => available.has(e)).map((e) => `create extension if not exists ${e};`).join(" ");
    await own.sql.unsafe(`${extensions} create type mood as enum ('ok', 'sad'); create type pair as (a int4, b text); create domain posint as int4 check (value > 0); create type shape as (p point, r int4range, m money); create domain cash as money; create type semver as (major int4, minor int4); create type public.point as (x int4);\n${tables(cases)}`).simple();
    const problems = await check(project(own.url, ["postgres"]), cases, (t, [, column]) => [
      `select c, to_json(c) as j, json_build_object('v', c) as o, jsonb_build_object('v', c) as b, json_build_array(c) as a, row_to_json(t) as r from ${t} t`,
      `select json_agg(c) as a, jsonb_agg(c) as b, json_object_agg(id, c) as o from ${t}`,
      // A column and a key named __proto__, which a literal or an assignment would take for the object's prototype.
      ...(column === "int4" ? [`select c as "__proto__", json_build_object('__proto__', c) as o, json_build_object('__proto__', json_build_object('__proto__', c)) as n from ${t}`] : []),
    ], async (q) => q.run(own.sql));
    assert.deepEqual(problems, [], problems.join("\n"));
  } finally {
    await closeSessions();
    await own.drop();
  }
});

const MYSQL: readonly Case[] = [
  ["tinyint", "tinyint", ["-128", "127"]],
  ["tinyint unsigned", "tinyint unsigned", ["0", "255"]],
  ["smallint", "smallint", ["-32768", "32767"]],
  ["mediumint unsigned", "mediumint unsigned", ["16777215"]],
  ["int", "int", ["-2147483648", "2147483647"]],
  ["int unsigned", "int unsigned", ["4294967295"]],
  ["bigint", "bigint", ["-9223372036854775808", "9223372036854775807"]],
  ["bigint unsigned", "bigint unsigned", ["0", "18446744073709551615"]],
  ["tinyint(1)", "tinyint(1)", ["0", "1"]],
  ["tinyint(1) of 0 or 1", "tinyint(1) check (c in (0, 1))", ["0", "1"]],
  ["boolean", "boolean", ["true", "false"]],
  ["decimal(10,2)", "decimal(10,2)", ["-99999999.99", "0.01"]],
  ["decimal(65,30)", "decimal(65,30)", ["99999999999999999999999999999999999.999999999999999999999999999999"]],
  ["float", "float", ["-3.40282e38", "1.17549e-38"]],
  ["double", "double", ["-1.7976931348623157e308", "2.2250738585072014e-308"]],
  ["bit(1)", "bit(1)", ["b'0'", "b'1'"]],
  ["bit(64)", "bit(64)", ["b'1111111111111111111111111111111111111111111111111111111111111111'"]],
  ["char(3)", "char(3)", ["''", "'abc'"]],
  ["varchar(10)", "varchar(10)", ["'hi'"]],
  ["text", "text", ["'hi'"]],
  ["enum", "enum('a','b')", ["'a'", "'b'"]],
  ["set", "set('a','b')", ["''", "'a,b'"]],
  ["binary(2)", "binary(2)", ["x'0102'"]],
  ["varbinary(4)", "varbinary(4)", ["x''", "x'0a'"]],
  // MySQL breaks bytes' base64 inside a document onto a new line every 76 characters.
  ["blob", "blob", ["x'ff'", "repeat('a', 100)"]],
  ["tinyblob", "tinyblob", ["x'00'"]],
  ["date", "date", ["'1000-01-01'", "'9999-12-31'"]],
  ["datetime(6)", "datetime(6)", ["'1000-01-01 00:00:00'", "'9999-12-31 23:59:59.999999'"]],
  ["timestamp(6)", "timestamp(6) null", ["'1970-01-01 00:00:01'", "'2038-01-19 03:14:07.999999'"]],
  ["time(6)", "time(6)", ["'-838:59:59'", "'838:59:59'", "'10:00:00.5'"]],
  ["year", "year", ["1901", "2155"]],
  ["json", "json", [`'{"a":1}'`, "'null'", "'[1]'", "'1.5'"]],
  // MySQL refuses a geometry in an aggregate's JSON.
  ["geometry", "geometry", ["ST_GeomFromText('POINT(1 2)')", "ST_GeomFromText('LINESTRING(0 0, 1 1)')"]],
  ["point", "point", ["ST_GeomFromText('POINT(1 2)')"]],
];

test("MySQL: every type's values are of the type the module declares, as a column and inside each document MySQL builds", { skip: mysqlLive }, async () => {
  const own = await mysqlDatabase("reylo_output_schema", `set time_zone = '+00:00';\n${tables(MYSQL)}`);
  try {
    const problems = await check(project(own.url, ["mysql2"]), MYSQL, (t, [, column]) => [
      `select c, json_object('v', c) as o, json_array(c) as a from ${t}`,
      ...(column === "geometry" || column === "point" ? [] : [`select json_arrayagg(c) as a, json_objectagg(id, c) as o from ${t}`]),
      ...(column === "int" ? [`select c as \`__proto__\`, json_object('__proto__', c) as o from ${t}`] : []),
    ], async (q) => q.run(own.db));
    assert.deepEqual(problems, [], problems.join("\n"));
  } finally {
    await closeSessions();
    await own.drop();
  }
});

const SQLITE: readonly Case[] = [
  ["integer", "integer", ["-9007199254740991", "9007199254740991"]],
  // Outside a STRICT table an INTEGER column keeps a REAL it cannot convert, one with a fraction or
  // past 64 bits, and stores the text '1.5' as that REAL: read alone, passed on, computed from and
  // inside a document SQLite builds.
  ["integer holding a REAL", "integer", ["0.5", "'1.5'", "1e20"]],
  ["bigint", "bigint", ["-9223372036854775808", "9223372036854775807"]],
  ["real", "real", ["-1.7976931348623157e308", "5e-324", "-0.0"]],
  ["text", "text", ["''", "'hi'"]],
  ["varchar(10)", "varchar(10)", ["'hi'"]],
  ["numeric", "numeric", ["1", "1.5"]],
  ["decimal(10,2)", "decimal(10,2)", ["1.25"]],
  ["boolean", "boolean", ["0", "1"]],
  ["date", "date", ["'2026-01-01'"]],
  ["datetime", "datetime", ["'2026-01-01 10:00:00'"]],
  ["json", "json", [`'{"a":1}'`, "'[1]'"]],
  ["untyped", "", ["1", "'hi'", "1.5"]],
  // No JSON function takes a BLOB.
  ["blob", "blob", ["x''", "x'00ff'"]],
];

test("SQLite: every type's values are of the type the module declares, as a column and inside each document SQLite builds", async () => {
  const dir = project("file:./app.db");
  const own = new DatabaseSync(join(dir, "app.db"));
  own.exec(tables(SQLITE, "integer primary key"));
  try {
    const problems = await check(dir, SQLITE, (t, [name, column]) => column === "blob" ? [`select c from ${t}`] : [
      `select c, json_object('v', c) as o, json_array(c) as a, json_quote(c) as q from ${t}`,
      `select json_group_array(c) as a, json_group_object(cast(id as text), c) as o from ${t}`,
      ...(column === "integer" ? [`select c as "__proto__", json_object('__proto__', c) as o from ${t}`] : []),
      ...(name === "integer holding a REAL" ? [
        `select coalesce(c, 0) as k, max(c, 0) as w, c + 1 as p, -c as m, abs(c) as b, iif(id > 0, c, 0) as i, (select c) as s, json_object('v', c + 1) as o from ${t}`,
        `select sum(c) as s, max(c) as x, min(c) as n, json_group_array(coalesce(c, 0)) as a from ${t}`,
        `select max(c) over () as w, lag(c) over (order by id) as l from ${t}`,
        `select case when c is null then json_object('v', id) else json_object('v', c) end as o, case when c is null then json_array(id) else json_array(c) end as a, json_group_object(cast(id as text), c) over () as m from ${t}`,
      ] : []),
    ], async (q) => q.run(own));
    assert.deepEqual(problems, [], problems.join("\n"));
  } finally {
    own.close();
  }
});

const CLICKHOUSE: readonly Case[] = [
  ["UInt8", "UInt8", ["0", "255"]],
  ["UInt32", "UInt32", ["4294967295"]],
  ["UInt64", "UInt64", ["18446744073709551615"]],
  ["UInt128", "UInt128", ["340282366920938463463374607431768211455"]],
  ["UInt256", "UInt256", ["115792089237316195423570985008687907853269984665640564039457584007913129639935"]],
  ["Int8", "Int8", ["-128", "127"]],
  ["Int32", "Int32", ["-2147483648", "2147483647"]],
  ["Int64", "Int64", ["-9223372036854775808", "9223372036854775807"]],
  ["Int128", "Int128", ["-170141183460469231731687303715884105728"]],
  ["Int256", "Int256", ["-57896044618658097711785492504343953926634992332820282019728792003956564819968"]],
  ["Float32", "Float32", ["-3.4028235e38", "1e-45"]],
  ["Float64", "Float64", ["-1.7976931348623157e308", "5e-324"]],
  ["Decimal(9,2)", "Decimal(9,2)", ["-9999999.99", "0.01"]],
  ["Decimal(76,38)", "Decimal(76,38)", ["-99999999999999999999999999999999999999.99999999999999999999999999999999999999"]],
  ["Bool", "Bool", ["true", "false"]],
  ["String", "String", ["''", "'hi'"]],
  ["FixedString(3)", "FixedString(3)", ["'abc'", "'a'"]],
  ["UUID", "UUID", ["'00000000-0000-0000-0000-000000000000'"]],
  ["Date", "Date", ["'1970-01-01'", "'2149-06-06'"]],
  ["Date32", "Date32", ["'1900-01-01'", "'2299-12-31'"]],
  ["DateTime", "DateTime('UTC')", ["'1970-01-01 00:00:00'", "'2106-02-07 06:28:15'"]],
  ["DateTime64(6)", "DateTime64(6, 'UTC')", ["'1900-01-01 00:00:00'", "'2299-12-31 23:59:59.999999'"]],
  ["Enum8", "Enum8('a' = 1, 'b' = 2)", ["'a'"]],
  ["IPv4", "IPv4", ["'10.0.0.1'"]],
  ["IPv6", "IPv6", ["'::1'"]],
  ["Array(Int32)", "Array(Int32)", ["[]", "[1, 2]"]],
  ["Array(Nullable(String))", "Array(Nullable(String))", ["['a', NULL]"]],
  ["Tuple(Int32, String)", "Tuple(Int32, String)", ["(1, 'a')"]],
  ["Tuple(a Int32, b String)", "Tuple(a Int32, b String)", ["(1, 'a')"]],
  ["Map(String, Int64)", "Map(String, Int64)", ["map('a', 9223372036854775807)", "map()"]],
  ["LowCardinality(String)", "LowCardinality(String)", ["'a'"]],
  ["Point", "Point", ["(1.5, -2)"]],
  ["Ring", "Ring", ["[(0, 0), (1, 1)]"]],
  ["Variant(Array(UInt8), String)", "Variant(Array(UInt8), String)", ["[1, 2]", "'a'"]],
  ["Variant(UInt64, Ring)", "Variant(UInt64, Ring)", ["18446744073709551615", "[(0, 0)]"]],
];

test("ClickHouse: every type's values are of the type the module declares, as a column", { skip: clickhouseLive }, async () => {
  const setup = CLICKHOUSE.map(([, column, values], i) => `CREATE TABLE t${i} (id Int32, c Nullable(${column})) ENGINE = MergeTree ORDER BY id;\nINSERT INTO t${i} VALUES ${[...values, "NULL"].map((v, n) => `(${n + 1}, ${v})`).join(", ")};`);
  // A type that cannot sit inside Nullable stands as itself, its NULL row left out.
  const own = await clickhouseDatabase("reylo_output_schema", setup.map((s, i) => (/^(Array|Tuple|Map|LowCardinality|Point|Ring|Variant)/.test(CLICKHOUSE[i]![1]) ? s.replace(/Nullable\((.*)\)\) ENGINE/, "$1) ENGINE").replace(/, \(\d+, NULL\);$/, ";") : s)).join("\n"));
  try {
    const problems = await check(project(own.url, ["@clickhouse/client"]), CLICKHOUSE, (t, [, column]) => [`SELECT c FROM ${t} ORDER BY id`, ...(column === "Int32" ? [`SELECT c AS \`__proto__\` FROM ${t} ORDER BY id`] : [])], async (q) => q.run(own.client));
    assert.deepEqual(problems, [], problems.join("\n"));
  } finally {
    await own.drop();
  }
});
