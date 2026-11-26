// What an app's bundle holds of the run time: the half of the dialect its queries are written for,
// and no other dialect's drivers or error codes (src/execute.ts, `Runtime`); and what only some
// statements need, `js.codecs` (src/codecs.ts) and Postgres's rarer types (src/postgres-types.ts),
// only where a module passes it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { build } from "esbuild";
import { DEFINE } from "../src/loader.ts";
import type { Dialect } from "../src/execute.ts";
import { ROOT } from "./helpers.ts";

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
  "Postgres's ranges": /whose lower is a bound|leave it out for an unbounded/,
  "pgvector": /pgvector/,
  "SQLite's synchronous clients": /This sql\.js binds|SQLite Wasm binds/,
  "Knex": /Knex/,
};

/** What each dialect's bundle of `.run()` on a query of plain columns may hold of `ELSEWHERE`. */
const HELD: Record<Dialect, string[]> = {
  postgres: ["Knex", "pgvector"],
  mysql: ["Knex"],
  sqlite: ["Knex", "SQLite's synchronous clients"],
  clickhouse: [],
};

/** A bundle of `module`, the text a build step writes, which runs its query with `.run()`: the files it holds and its text. */
async function bundle(module: string): Promise<{ files: string[]; text: string }> {
  const contents = `${module}\nexport const go = (client) => q.run(client, {});\n`;
  const { metafile, outputFiles } = await build({ stdin: { contents, resolveDir: ROOT }, bundle: true, minify: true, platform: "node", format: "esm", write: false, metafile: true, alias: { reylo: join(ROOT, "src", "index.ts") }, logLevel: "silent" });
  const files = Object.entries(Object.values(metafile.outputs)[0]!.inputs).flatMap(([path, input]) => (input.bytesInOutput > 0 ? [path] : []));
  return { files, text: outputFiles[0]!.text };
}

test("a bundle of a query holds its dialect's drivers and error codes, and no other dialect's", async () => {
  for (const dialect of Object.keys(OWN) as Dialect[]) {
    // The module a build step writes, as src/loader.ts writes it.
    const { files, text } = await bundle(`import { ${DEFINE[dialect]} } from "reylo";\nconst q = ${DEFINE[dialect]}({ sql: "select 1 as n", params: [], cardinality: "one", columns: [["n", "int"]] });`);
    assert.deepEqual(files.filter((path) => /^src\/(decode|error-codes)/.test(path)).sort(), OWN[dialect].sort(), dialect);
    // Neither js.codecs nor Postgres's rarer types, which no module passed.
    for (const path of ["src/codecs.ts", "src/geojson.ts", "src/postgres-types.ts"]) assert.ok(!files.includes(path), `${dialect} holds ${path}`);
    for (const [part, marker] of Object.entries(ELSEWHERE)) assert.equal(marker.test(text), HELD[dialect].includes(part), `${dialect}: ${part}`);
  }
});

test("a bundle holds js.codecs and Postgres's rarer types where a module passes them", async () => {
  const coded = await bundle(`import { definePostgresQuery, codecRuntime } from "reylo";\nconst q = definePostgresQuery({ sql: "select now() as at", params: [], columns: [["at", "timestamptz"]], codecs: { at: "instant" }, codecRuntime });`);
  assert.ok(coded.files.includes("src/codecs.ts"));
  const typed = await bundle(`import { definePostgresQuery, postgresTypes } from "reylo";\nconst q = definePostgresQuery({ sql: "select point(1, 2) as p", params: [], columns: [["p", "point"]], postgresTypes });`);
  assert.ok(typed.files.includes("src/postgres-types.ts"));
  assert.match(typed.text, /lc_monetary/);
});

test("a query of Postgres's rarer types reads them with the postgresTypes its module passes, in a process of its own, and refuses them without", async () => {
  const decode = `export const go = () => JSON.stringify(q["~standard"].query.decode([["(1.5,-2)"]]));`;
  const run = async (module: string) => {
    const { outputFiles } = await build({ stdin: { contents: `${module}\n${decode}\n`, resolveDir: ROOT }, bundle: true, platform: "node", format: "esm", write: false, alias: { reylo: join(ROOT, "src", "index.ts") }, logLevel: "silent" });
    const script = `${outputFiles[0]!.text}\ntry { console.log(go()); } catch (e) { console.log(e.message); }`;
    return spawnSync(process.execPath, ["--input-type=module", "-e", script], { encoding: "utf8" }).stdout.trim();
  };
  assert.equal(await run(`import { definePostgresQuery, postgresTypes } from "reylo";\nconst q = definePostgresQuery({ sql: "select p from t", params: [], cardinality: "one", columns: [["p", "point"]], postgresTypes });`), '{"p":{"x":1.5,"y":-2}}');
  assert.match(await run(`import { definePostgresQuery } from "reylo";\nconst q = definePostgresQuery({ sql: "select p from t", params: [], cardinality: "one", columns: [["p", "point"]] });`), /^Reylo reads and writes a Postgres point with Reylo's postgresTypes, which a module a build step writes passes where its statement holds one/);
});
