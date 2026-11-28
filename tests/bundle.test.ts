// What an app's bundle holds of the run time: the half of the dialect its queries are written for,
// and no other dialect's drivers or error codes (src/execute.ts, `Runtime`); on Postgres, the
// drivers and the readers its modules pass and no other (src/installed-drivers.ts, src/values.ts,
// `allReaders`); and what only some statements need, `js.codecs` (src/codecs.ts) and Postgres's
// rarer types (src/postgres-types.ts), only where a module passes it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { build } from "esbuild";
import type { Dialect } from "../src/execute.ts";
import { DRIVERS, ROOT } from "./helpers.ts";

/** Each dialect's drivers and error codes. MySQL's include Bun.sql's reader of rows, which Postgres's module holds (`bunRows`). */
const OWN: Record<Dialect, string[]> = {
  postgres: ["src/decode.ts", "src/error-codes-postgres.ts"],
  mysql: ["src/decode-mysql.ts", "src/decode.ts", "src/error-codes-mysql.ts"],
  sqlite: ["src/decode-sqlite.ts", "src/error-codes-sqlite.ts"],
  clickhouse: ["src/decode-clickhouse.ts", "src/error-codes-clickhouse.ts"],
};

/** Text only another dialect's half of the run time, or a part a module passes, holds: a bundle without them holds none of it. */
const ELSEWHERE: Record<string, RegExp> = {
  "Postgres's money check": /lc_monetary/,
  "Postgres's hstore": /is an hstore/,
  "Postgres's row values": /whose field/,
  "Postgres's ranges": /is a bound, never null/,
  "Postgres's money": /not the C locale's form/,
  "pgvector": /pgvector/,
  "SQLite's synchronous clients": /This sql\.js binds|SQLite Wasm binds/,
  "Knex": /Knex/,
  "Slonik": /Slonik/,
  "PGlite": /PGlite/,
  "postgres.js": /postgres\.js/,
  "Bun.sql": /Bun\.sql/,
  "pg-promise": /pg-promise/,
  "Postgres's catalog reading": /pg_catalog\.pg_attribute/,
  "Postgres's date and time readers": /DateStyle/,
};

/** What each dialect's bundle of `.run()` on a query of plain columns may hold of `ELSEWHERE`: on Postgres, on pg alone, nothing but pgvector's parameters. */
const HELD: Record<Dialect, string[]> = {
  postgres: ["pgvector"],
  mysql: ["Knex", "Bun.sql"],
  sqlite: ["Knex", "Bun.sql", "SQLite's synchronous clients"],
  clickhouse: [],
};

/** "reylo/module", which a module a build step writes imports, from the sources. */
const ALIAS = { "reylo/module": join(ROOT, "src", "module.ts") };

/** The module a build step writes for a statement of one int column, on Postgres on pg alone. */
const MODULE: Record<Dialect, string> = {
  postgres: `import { definePostgresQuery, readNumber, pgDriver } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select 1 as n", params: [], cardinality: "one", columns: [["n", "int"]], readers: { int: readNumber }, drivers: [pgDriver] });`,
  mysql: `import { defineMysqlQuery } from "reylo/module";\nconst q = defineMysqlQuery({ sql: "select 1 as n", params: [], cardinality: "one", columns: [["n", "int"]] });`,
  sqlite: `import { defineSqliteQuery } from "reylo/module";\nconst q = defineSqliteQuery({ sql: "select 1 as n", params: [], cardinality: "one", columns: [["n", "int"]] });`,
  clickhouse: `import { defineClickhouseQuery } from "reylo/module";\nconst q = defineClickhouseQuery({ sql: "select 1 as n", params: [], cardinality: "one", columns: [["n", "int"]] });`,
};

/** A bundle of `module`, the text a build step writes, which runs its query with `.run()`: the files it holds and its text. */
async function bundle(module: string): Promise<{ files: string[]; text: string }> {
  const contents = `${module}\nexport const go = (client) => q.run(client, {});\n`;
  const { metafile, outputFiles } = await build({ stdin: { contents, resolveDir: ROOT }, bundle: true, minify: true, platform: "node", format: "esm", write: false, metafile: true, alias: ALIAS, logLevel: "silent" });
  const files = Object.entries(Object.values(metafile.outputs)[0]!.inputs).flatMap(([path, input]) => (input.bytesInOutput > 0 ? [path] : []));
  return { files, text: outputFiles[0]!.text };
}

test("a bundle of a query holds its dialect's drivers and error codes, and no other dialect's", async () => {
  for (const dialect of Object.keys(OWN) as Dialect[]) {
    // The module a build step writes, as src/loader.ts writes it.
    const { files, text } = await bundle(MODULE[dialect]);
    assert.deepEqual(files.filter((path) => /^src\/(decode|error-codes)/.test(path)).sort(), OWN[dialect].sort(), dialect);
    // Neither js.codecs nor Postgres's rarer types, which no module passed.
    for (const path of ["src/codecs.ts", "src/geojson.ts", "src/postgres-types.ts"]) assert.ok(!files.includes(path), `${dialect} holds ${path}`);
    for (const [part, marker] of Object.entries(ELSEWHERE)) assert.equal(marker.test(text), HELD[dialect].includes(part), `${dialect}: ${part}`);
  }
});

test("a bundle holds js.codecs and Postgres's rarer types where a module passes them", async () => {
  const coded = await bundle(`import { definePostgresQuery, codecRuntime, readTimestamptz, pgDriver } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select now() as at", params: [], columns: [["at", "timestamptz"]], codecs: { at: "instant" }, codecRuntime, readers: { timestamptz: readTimestamptz }, drivers: [pgDriver] });`);
  assert.ok(coded.files.includes("src/codecs.ts"));
  const typed = await bundle(`import { definePostgresQuery, postgresTypes, pgDriver } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select point(1, 2) as p", params: [], columns: [["p", "point"]], postgresTypes, drivers: [pgDriver] });`);
  assert.ok(typed.files.includes("src/postgres-types.ts"));
  assert.match(typed.text, /lc_monetary/);
});

test("a query of Postgres's rarer types reads them with the postgresTypes its module passes, in a process of its own, and refuses them without", async () => {
  const decode = `export const go = () => JSON.stringify(q["~standard"].query.decode([["(1.5,-2)"]]));`;
  const run = async (module: string) => {
    const { outputFiles } = await build({ stdin: { contents: `${module}\n${decode}\n`, resolveDir: ROOT }, bundle: true, platform: "node", format: "esm", write: false, alias: ALIAS, logLevel: "silent" });
    return fresh(`${outputFiles[0]!.text}\ntry { console.log(go()); } catch (e) { console.log(e.message); }`);
  };
  assert.equal(await run(`import { definePostgresQuery, postgresTypes } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select p from t", params: [], cardinality: "one", columns: [["p", "point"]], postgresTypes });`), '{"p":{"x":1.5,"y":-2}}');
  assert.match(await run(`import { definePostgresQuery } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select p from t", params: [], cardinality: "one", columns: [["p", "point"]] });`), /^Reylo reads and writes a Postgres point with Reylo's postgresTypes, which a module a build step writes passes where its statement holds one/);
});

/** What `script` prints, run in a process of its own, where nothing has installed Postgres's rarer types. */
function fresh(script: string): string {
  // On standard input, since a bundle is past the length a command line takes on Windows.
  const out = spawnSync(process.execPath, ["--input-type=module"], { input: script, encoding: "utf8" });
  // A process that fails to start or crashes is the test's failure, never a value it prints.
  if (out.error) throw out.error;
  if (out.status !== 0) throw new Error(`The script exited with ${out.status}: ${out.stderr}`);
  return out.stdout.trim();
}

test("a kind needs Postgres's rarer types exactly where the module a build step writes imports them for it", () => {
  // Each kind with a value of it as Postgres writes it: a module imports postgresTypes for a kind
  // holdsRareKind names, and the run time reads a value of every other kind without them.
  const values: [kind: string, text: string][] = [
    ["int", "1"], ["int8", "1"], ["text", "x"], ["bool", "t"], ["decimal", "1.5"], ["float", "NaN"], ["json", "{}"], ["bytes", "\\x00"], ["vector", "[1]"],
    ["date", "2026-01-02"], ["timestamp", "2026-01-02 03:04:05"], ["timestamptz", "2026-01-02 03:04:05+00"], ["interval", "1 day"], ["int[]", "{1,2}"], ["text[][]", "{{a}}"],
    ["point", "(1,2)"], ["line", "{1,-1,0}"], ["lseg", "[(0,0),(1,1)]"], ["box", "(3,4),(1,2)"], ["path", "((0,0),(1,1))"], ["polygon", "((0,0),(1,1))"], ["circle", "<(1,2),3>"],
    ["box[]", "{(3,4),(1,2)}"], ["hstore", '"a"=>"1"'], ["money", "$1.00"], ["money[]", '{"$1.00"}'], ["range:int", "[1,5)"], ["multirange:int8", "{[1,3)}"], ["range:timestamptz[]", '{"[2026-01-01 00:00:00+00,)"}'],
    ["{int[]}[]", '{"{1,2}",NULL}'],
    ['(["a","int"])', "(1)"], ['(["a","int"])[]', '{"(1)"}'], ['(["p","text"],["q","int[]"])', '(x,"{1}")'],
  ];
  const values_ts = JSON.stringify(pathToFileURL(join(ROOT, "src", "values.ts")).href);
  const out = fresh(`const { holdsRareKind, textDecoder } = await import(${values_ts});
for (const [kind, text] of ${JSON.stringify(values)}) {
  let needs = false;
  const decode = textDecoder(kind);
  try { decode(text); } catch (e) { needs = /postgresTypes/.test(e.message); }
  console.log(kind, holdsRareKind(kind) === needs ? "ok" : "holds " + holdsRareKind(kind) + ", needs " + needs);
}`);
  assert.deepEqual(out.split("\n").filter((line) => !line.endsWith(" ok")), [], out);
});

test("PGlite reads a statement of plain columns in a process where nothing installed Postgres's rarer types, and refuses one of them before it runs", async () => {
  const run = async (module: string) => {
    const { outputFiles } = await build({ stdin: { contents: `${module}\nexport { q };\n`, resolveDir: ROOT }, bundle: true, platform: "node", format: "esm", write: false, alias: ALIAS, logLevel: "silent" });
    const pglite = JSON.stringify(pathToFileURL(join(DRIVERS, "@electric-sql", "pglite", "dist", "index.js")).href);
    return fresh(`${outputFiles[0]!.text}\nconst { PGlite } = await import(${pglite});\nconst db = new PGlite();\ntry { console.log(JSON.stringify(await q.run(db))); } catch (e) { console.log(e.message); }`);
  };
  assert.equal(await run(`import { definePostgresQuery, readNumber, pgliteDriver } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select 1::int as n, 'x' as t", params: [], cardinality: "one", columns: [["n", "int"], ["t", "text"]], readers: { int: readNumber }, drivers: [pgliteDriver] });`), '{"n":1,"t":"x"}');
  // A JSON mask that holds no work asks for no rarer type.
  assert.equal(await run(`import { definePostgresQuery, readNumber, pgliteDriver } from "reylo/module";\nconst q = definePostgresQuery({ sql: "select 1::int as n", params: [], cardinality: "one", columns: [["n", "int"]], nonFinite: [null], readers: { int: readNumber }, drivers: [pgliteDriver] });`), '{"n":1}');
  assert.match(await run(`import { definePostgresQuery, pgliteDriver } from "reylo/module";\nconst q = definePostgresQuery({ sql: "insert into nowhere default values returning point(1, 2) as p", params: [], cardinality: "one", columns: [["p", "point"]], source: "p.sql", drivers: [pgliteDriver] });`), /^p\.sql: Reylo reads and writes a Postgres point with Reylo's postgresTypes/);
});

test("a reader a later module installs reads the values of a decoder built before it, as a driver's table keeps one by type", () => {
  const values_ts = JSON.stringify(pathToFileURL(join(ROOT, "src", "values.ts")).href);
  const out = fresh(`const { installReaders, readJson, textDecoder } = await import(${values_ts});
const json = textDecoder("json");
const before = json('{"a":1}');
installReaders({ json: readJson });
console.log(JSON.stringify([before, json('{"a":1}'), textDecoder("text")("x")]));`);
  assert.equal(out, '["{\\"a\\":1}",{"a":1},"x"]');
});
