import { test } from "node:test";
import { spawnSync } from "node:child_process";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { Session } from "../src/analyze.ts";
import { placeRefusal, projectDrivers, SQLEAN_AGGREGATES, SQLEAN_FUNCTIONS, withoutSqlean } from "../src/sqlite-drivers.ts";
import { DRIVERS } from "./helpers.ts";

// The drivers are the labs' (lab/drivers), linked into each project as an install would put them.
// libSQL carries SQLite 3.45.1, better-sqlite3 3.53 and sqlite3 3.52: `json_pretty` came in 3.46
// and `iif` with two arguments in 3.48.
const missing = ["better-sqlite3", "@libsql/client", "sqlite3"].find((d) => !existsSync(join(DRIVERS, d)));
const skip = missing ? `lab/drivers is not installed (no ${missing})` : false;

function project(drivers: string[]): { root: string; env: Record<string, string> } {
  const root = mkdtempSync(join(tmpdir(), "reylo-drivers-"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ dependencies: Object.fromEntries(drivers.map((d) => [d, "*"])) }));
  for (const d of drivers) link(root, d);
  const db = new DatabaseSync(join(root, "app.db"));
  db.exec("create table notes (id integer primary key, body text not null, meta text)");
  db.close();
  return { root, env: { DATABASE_URL: "file:./app.db" } };
}

function link(root: string, driver: string) {
  const to = join(root, "node_modules", driver);
  mkdirSync(dirname(to), { recursive: true });
  symlinkSync(join(DRIVERS, driver), to);
}

async function analyze(root: string, env: Record<string, string>, sql: string) {
  const session = await Session.open(root, env);
  try {
    const outcome = await session.analyze(sql);
    assert.ok(outcome.ok, !outcome.ok ? outcome.message : "");
    return outcome;
  } finally {
    await session.close();
  }
}

const PRETTY = "select id, json_pretty(meta) as meta from notes where id = :id";
const IIF = "select id, iif(id > 1, 'big') as size from notes";

test("a statement the app's SQLite refuses fails the type check at the call, with that SQLite's message", { skip }, async () => {
  const { root, env } = project(["@libsql/client"]);
  const pretty = await analyze(root, env, PRETTY);
  assert.deepEqual(pretty.analysis.diagnostics, [
    { message: "@libsql/client runs SQLite 3.45.1, which refuses this statement: no such function: json_pretty", start: PRETTY.indexOf("json_pretty"), length: "json_pretty".length, code: 6 },
  ]);
  const iif = await analyze(root, env, IIF);
  assert.deepEqual(iif.analysis.diagnostics, [
    { message: "@libsql/client runs SQLite 3.45.1, which refuses this statement: wrong number of arguments to function iif()", start: IIF.indexOf("iif"), length: 3, code: 6 },
  ]);
  // What every SQLite takes is no error.
  assert.deepEqual((await analyze(root, env, "select id, iif(id > 1, 'big', 'small') as size from notes")).analysis.diagnostics, []);
});

test("drivers whose SQLite has the function prepare it, and a project that lists no driver is not checked", { skip }, async () => {
  for (const drivers of [["better-sqlite3", "sqlite3"], []]) {
    const { root, env } = project(drivers);
    assert.deepEqual(projectDrivers(root), drivers);
    for (const sql of [PRETTY, IIF]) assert.deepEqual((await analyze(root, env, sql)).analysis.diagnostics, [], `${drivers.join(", ") || "no driver"}: ${sql}`);
  }
});

test("an analysis read from reylo/.cache/ is checked on the drivers too", { skip }, async () => {
  const { root, env } = project([]);
  const first = await analyze(root, env, PRETTY);
  assert.deepEqual([first.source, first.analysis.diagnostics], ["database", []]);
  // The project takes libSQL: the entry still matches the schema, and the driver refuses it.
  writeFileSync(join(root, "package.json"), JSON.stringify({ dependencies: { "@libsql/client": "*" } }));
  link(root, "@libsql/client");
  const second = await analyze(root, env, PRETTY);
  assert.equal(second.source, "cache");
  assert.match(second.analysis.diagnostics[0]?.message ?? "", /^@libsql\/client runs SQLite 3\.45\.1, which refuses this statement: no such function: json_pretty$/);
});

test("a session whose connection was released checks a statement it reads from reylo/.cache/ all the same", { skip }, async () => {
  const { root, env } = project(["@libsql/client"]);
  const session = await Session.open(root, env);
  try {
    const first = await session.analyze(PRETTY);
    assert.ok(first.ok && first.analysis.diagnostics.length === 1);
    // A build step's session releases its connection once idle, while the schema it read is still trusted.
    await session.release();
    const second = await session.analyze(PRETTY);
    assert.ok(second.ok, !second.ok ? second.message : "");
    assert.equal(second.source, "cache");
    assert.match(second.analysis.diagnostics[0]?.message ?? "", /^@libsql\/client runs SQLite 3\.45\.1, which refuses this statement: no such function: json_pretty$/);
  } finally {
    await session.close();
  }
});

test("a refusal is placed at the call or the text it names, in UTF-8 bytes", () => {
  const sql = "-- café\nselect n.id from notes n right join notes m on m.id = n.id";
  const bytes = (s: string) => new TextEncoder().encode(s).length;
  assert.deepEqual(placeRefusal(sql, 'near "right": syntax error'), { start: bytes(sql.slice(0, sql.indexOf("right"))), length: 5 });
  assert.deepEqual(placeRefusal("select JSON_PRETTY (x) from t", "no such function: json_pretty"), { start: 7, length: 11 });
  // A message that names nothing in the statement points at its first word.
  assert.deepEqual(placeRefusal("  select 1", "unrecognized token"), { start: 2, length: 6 });
});

// Cloudflare D1 through the project's wrangler, which runs it locally in workerd: D1 refuses
// functions its SQLite has (`json_pretty`, the alias `ceiling` where `ceil` is allowed), and says so
// only when a statement compiles.
const d1 = skip || (!existsSync(join(DRIVERS, "wrangler")) && "lab/drivers has no wrangler installed");

function worker(root: string, d1Binding: boolean, durableObject: false | "sqlite" | "kv" | "exports" = false, services = false) {
  const config = {
    name: "reylo-test",
    main: "worker.js",
    compatibility_date: "2026-10-01",
    // Bindings to Workers that aren't running locally, listed before D1: wrangler's proxy calls the
    // Worker on any read of such a binding's properties, and that call throws.
    ...(services && { services: [{ binding: "OTHER", service: "reylo-test-other" }, { binding: "ENTRY", service: "reylo-test-other", entrypoint: "Admin" }] }),
    ...(d1Binding && { d1_databases: [{ binding: "DB", database_name: "reylo-test", database_id: "00000000-0000-0000-0000-000000000000" }] }),
    ...(durableObject && { durable_objects: { bindings: [{ name: "BOARD", class_name: "Board" }] } }),
    // The class declared by migrations, or by the declarative `exports` that replaces them.
    ...(durableObject === "exports" ? { exports: { Board: { type: "durable-object", storage: "sqlite" } } } : durableObject && { migrations: [{ tag: "v1", [durableObject === "sqlite" ? "new_sqlite_classes" : "new_classes"]: ["Board"] }] }),
  };
  writeFileSync(join(root, "wrangler.jsonc"), JSON.stringify(config));
  const board = durableObject ? `import { DurableObject } from "cloudflare:workers";\nexport class Board extends DurableObject {}\n` : "";
  writeFileSync(join(root, "worker.js"), `${board}export default { fetch() { return new Response(''); } };\n`);
}

test("a statement Cloudflare D1 refuses fails the type check at the call, with D1's message", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, true);
  const session = await Session.open(root, env);
  try {
    const CEILING = "select id, ceiling(id / 2.0) as half from notes";
    for (const [sql, fn] of [[PRETTY, "json_pretty"], [CEILING, "ceiling"]] as const) {
      const outcome = await session.analyze(sql);
      assert.ok(outcome.ok);
      const [diagnostic, ...rest] = outcome.analysis.diagnostics;
      assert.deepEqual(rest, []);
      assert.match(diagnostic?.message ?? "", new RegExp(`^Cloudflare D1, as wrangler \\d+\\.\\d+\\.\\d+ runs it locally, refuses this statement: not authorized to use function: ${fn}$`));
      assert.deepEqual([diagnostic?.start, diagnostic?.length, diagnostic?.code], [sql.indexOf(fn), fn.length, 6]);
    }
    // What D1 allows compiles against the schema the type check read, its parameters bound.
    for (const sql of ["select id, ceil(id / 2.0) as half from notes where id = :id", "insert into notes (body, meta) values (:body, :meta) returning id", IIF]) {
      const outcome = await session.analyze(sql);
      assert.ok(outcome.ok);
      assert.deepEqual(outcome.analysis.diagnostics, [], sql);
    }
  } finally {
    await session.close();
  }
});

test("D1 is found by its binding's name, beside service bindings to Workers that aren't running", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, true, false, true);
  assert.match((await analyze(root, env, PRETTY)).analysis.diagnostics[0]?.message ?? "", /^Cloudflare D1, as wrangler \d+\.\d+\.\d+ runs it locally, refuses this statement: not authorized to use function: json_pretty$/);
  // And so is a Durable Object's SQL storage, with no D1 database.
  worker(root, false, "sqlite", true);
  assert.match((await analyze(root, env, PRETTY)).analysis.diagnostics[0]?.message ?? "", /^A Durable Object's SQL storage, as wrangler \d+\.\d+\.\d+ runs it locally, refuses this statement: not authorized to use function: json_pretty$/);
});

// A wrangler of the test's own, whose D1 child ends at the start or under a statement. The check is
// left out, and the process that asked goes on to its end rather than ending with its wait unsettled.
test("a D1 child that ends before it is ready or under a statement leaves the check out, and the process that asked goes on", { timeout: 60_000 }, () => {
  const ending = {
    start: `throw new Error("workerd is missing");`,
    statement: `const statement = { bind: () => statement, all: () => process.exit(1), run: async () => ({}) };
export const unstable_readConfig = () => ({ d1_databases: [{ binding: "DB" }] });
export const getPlatformProxy = async () => ({ env: { DB: { prepare: () => statement, batch: async () => [] } }, dispose: async () => {} });`,
    // The first child is gone once it is ready, before the process that asked has seen it go: a
    // statement sent then meets a closed channel. The next one answers.
    ready: `import { existsSync, writeFileSync } from "node:fs";
const first = !existsSync("started");
writeFileSync("started", "");
const statement = { bind: () => statement, all: async () => { throw new Error("no such function: f"); }, run: async () => ({}) };
export const unstable_readConfig = () => ({ d1_databases: [{ binding: "DB" }] });
export const getPlatformProxy = async () => (first && setTimeout(() => process.exit(1), 50), { env: { DB: { prepare: () => statement, batch: async () => [] } }, dispose: async () => {} });`,
  };
  for (const [when, source] of Object.entries(ending)) {
    const root = mkdtempSync(join(tmpdir(), "reylo-d1-ends-"));
    writeFileSync(join(root, "package.json"), JSON.stringify({ devDependencies: { wrangler: "*" } }));
    writeFileSync(join(root, "wrangler.json"), "{}");
    mkdirSync(join(root, "node_modules", "wrangler"), { recursive: true });
    writeFileSync(join(root, "node_modules", "wrangler", "package.json"), JSON.stringify({ name: "wrangler", version: "0.0.0", type: "module", main: "index.js" }));
    writeFileSync(join(root, "node_modules", "wrangler", "index.js"), source);
    new DatabaseSync(join(root, "app.db")).close();
    // A file rather than `--eval`, whose `--input-type` the D1 child would be forked with too.
    writeFileSync(join(root, "check.mjs"), `const { openProjectDrivers } = await import(${JSON.stringify(new URL("../src/sqlite-drivers.ts", import.meta.url).href)});
const check = async (wait) => {
  const drivers = await openProjectDrivers(${JSON.stringify(root)}, ${JSON.stringify(join(root, "app.db"))});
  for (const end = Date.now() + wait; Date.now() < end; );
  return Promise.all(drivers.map((d) => d.refuses("select 1", 0)));
};
// A second connection, opened as soon as the first check is over, gets a child of its own.
console.log(JSON.stringify([await check(1000), await check(0)]));`);
    const run = spawnSync(process.execPath, [join(root, "check.mjs")], { encoding: "utf8", timeout: 50_000 });
    assert.equal(run.status, 0, `${when}: ${run.stderr}`);
    // A start that failed is not tried again for the same config.
    const expected = { start: [[], []], statement: [[null], [null]], ready: [[null], ["no such function: f"]] }[when];
    assert.deepEqual(JSON.parse(run.stdout), expected, when);
  }
});

test("a project whose wrangler config binds no D1 database is not asked, until its config binds one", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, false);
  assert.deepEqual((await analyze(root, env, PRETTY)).analysis.diagnostics, []);
  assert.deepEqual((await analyze(root, env, PRETTY)).analysis.diagnostics, []);
  // The failed start is remembered only as long as the config it read.
  worker(root, true);
  assert.match((await analyze(root, env, PRETTY)).analysis.diagnostics[0]?.message ?? "", /refuses this statement: not authorized to use function: json_pretty$/);
});

test("a project with a SQLite-backed Durable Object and no D1 database is asked as the object's SQL storage, which refuses what D1 refuses", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, false, "sqlite");
  const [diagnostic, ...rest] = (await analyze(root, env, PRETTY)).analysis.diagnostics;
  assert.deepEqual(rest, []);
  assert.match(diagnostic?.message ?? "", /^A Durable Object's SQL storage, as wrangler \d+\.\d+\.\d+ runs it locally, refuses this statement: not authorized to use function: json_pretty$/);
  assert.deepEqual((await analyze(root, env, "select id, ceil(id / 2.0) as half from notes where id = :id")).analysis.diagnostics, []);
});

test("a SQLite-backed Durable Object that exports declares is asked as well", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, false, "exports");
  assert.match((await analyze(root, env, PRETTY)).analysis.diagnostics[0]?.message ?? "", /^A Durable Object's SQL storage, as wrangler \d+\.\d+\.\d+ runs it locally, refuses this statement: not authorized to use function: json_pretty$/);
});

test("a project whose only Durable Object is key-value backed has no SQL storage to ask", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, false, "kv");
  assert.deepEqual((await analyze(root, env, PRETTY)).analysis.diagnostics, []);
});

test("D1 compiles each statement against the schema as it is now, after a migration and in a later session", { skip: d1, timeout: 120_000 }, async () => {
  const { root, env } = project(["wrangler"]);
  worker(root, true);
  const TAGGED = "select id, tag from notes where tag = :tag";
  const session = await Session.open(root, env);
  try {
    assert.deepEqual((await session.analyze("select id from notes")).ok, true);
    // A migration adds a column and a table while the session runs.
    const db = new DatabaseSync(join(root, "app.db"));
    db.exec("alter table notes add column tag text; create table tags (name text primary key)");
    db.close();
    assert.equal(await session.recheck(), true);
    for (const sql of [TAGGED, "select name from tags"]) {
      const outcome = await session.analyze(sql);
      assert.ok(outcome.ok, !outcome.ok ? outcome.message : "");
      assert.deepEqual(outcome.analysis.diagnostics, [], sql);
    }
  } finally {
    await session.close();
  }
  // A later session, as a build step opens after a pause, asks D1 again.
  const pretty = await analyze(root, env, "select json_pretty(tag) as t from notes");
  assert.match(pretty.analysis.diagnostics[0]?.message ?? "", /refuses this statement: not authorized to use function: json_pretty$/);
});

// Turso Cloud loads the sqlean extensions into every connection, and `@libsql/client` reaches it,
// while the libSQL it carries for a local file has none of them.
const SQLEAN = "select uuid4() as id, levenshtein(body, :q) as distance, regexp_like(body, 'a+') as matched from notes";

test("libSQL's drivers let the sqlean functions Turso Cloud loads through, and another SQLite's refuses them", { skip }, async () => {
  const libsql = project(["@libsql/client"]);
  assert.deepEqual((await analyze(libsql.root, libsql.env, SQLEAN)).analysis.diagnostics, []);
  const better = project(["better-sqlite3"]);
  assert.match((await analyze(better.root, better.env, SQLEAN)).analysis.diagnostics[0]?.message ?? "", /^better-sqlite3 runs SQLite [\d.]+, which refuses this statement: no such function: uuid4$/);
  // A function no SQLite has is still refused.
  assert.match((await analyze(libsql.root, libsql.env, "select nosuch(body) as x from notes")).analysis.diagnostics[0]?.message ?? "", /no such function: nosuch/);
  // The rest of a statement that calls one is still checked: libSQL's SQLite 3.45.1 has no `json_pretty`, and takes `iif` with three arguments or more.
  assert.match((await analyze(libsql.root, libsql.env, "select uuid4() as id, json_pretty(meta) as meta from notes")).analysis.diagnostics[0]?.message ?? "", /^@libsql\/client runs SQLite 3\.45\.1, which refuses this statement: no such function: json_pretty$/);
  assert.match((await analyze(libsql.root, libsql.env, "select levenshtein(body, 'a') as d, iif(id > 1, 'big') as size from notes")).analysis.diagnostics[0]?.message ?? "", /wrong number of arguments to function iif\(\)/);
  // An aggregate keeps a query that needs one valid, and a quoted or commented call is still a call.
  for (const sql of ["select stats_stddev(id) filter (where id > 0) as s from notes", "select stats_stddev(id) as s from notes having count(*) > 0", 'select "uuid4"() as id from notes', "select uuid4 /* id */ () as id from notes"]) {
    assert.deepEqual((await analyze(libsql.root, libsql.env, sql)).analysis.diagnostics, [], sql);
  }
});

test("the check reads a call of a sqlean function as one of char, outside strings, quoted names and comments", () => {
  assert.equal(withoutSqlean("select uuid4(), 'uuid4()', \"uuid4\", Uuid4 (x) -- uuid4()\nfrom t", "uuid4"), "select char(), 'uuid4()', \"uuid4\", char (x) -- uuid4()\nfrom t");
  assert.equal(withoutSqlean("select value from stats_seq(1, 3)", "stats_seq"), "select value from generate_series(1, 3)");
  // A name runs on past ASCII and over `$`, and a no-break space is a character of one.
  assert.equal(withoutSqlean("select éuuid4(), uuid4$(), uuid4\u00a0() from t", "uuid4"), undefined);
  // A vertical tab inside a run of whitespace is whitespace too.
  assert.equal(withoutSqlean("select uuid4 \v() from t", "uuid4"), "select char \v() from t");
  assert.equal(withoutSqlean("select 'it''s uuid4()' /* uuid4() */ as uuid4", "uuid4"), undefined);
  // A quoted name that is called, and a comment before the parenthesis.
  assert.equal(withoutSqlean('select "uuid4"(), [uuid4] /* a */ (), "uuid4" from t', "uuid4"), 'select char(), char /* a */ (), "uuid4" from t');
  assert.equal(withoutSqlean("select uuid4 -- a\n() from t", "uuid4"), "select char -- a\n() from t");
  // An aggregate stays one.
  assert.equal(withoutSqlean("select stats_stddev(id) filter (where id > 0) from t", "stats_stddev"), "select total(id) filter (where id > 0) from t");
  assert.equal(withoutSqlean("select stats_perc(id, 50) from t", "stats_perc"), "select group_concat(id, 50) from t");
});

test("the sqlean functions the driver check lets through are the ones the engine knows", () => {
  const walker = readFileSync(new URL("../engine/sqlite/csrc/reylo_sqlite.c", import.meta.url), "utf8");
  const table = walker.slice(walker.indexOf("aTurso[] = {"), walker.indexOf("};", walker.indexOf("aTurso[] = {")));
  const sqleans = table.slice(table.indexOf("/* sqlean's"), table.indexOf("/* Turso's own engine"));
  const sqlean = [...sqleans.matchAll(/\{"([a-z0-9_]+)",/g)].map((m) => m[1]!);
  assert.ok(sqlean.length > 100, `${sqlean.length}`);
  assert.deepEqual(sqlean.filter((name) => !SQLEAN_FUNCTIONS.has(name)), []);
  // Each aggregate, with its argument count, but `median` and `percentile`, which the engine's own SQLite has.
  const aggregates = [...sqleans.matchAll(/\{"([a-z0-9_]+)", (-?\d+), 'a'/g)].map((m) => [m[1]!, Number(m[2])] as const);
  assert.deepEqual(new Map(aggregates), new Map([...SQLEAN_AGGREGATES].filter(([name]) => name !== "median" && name !== "percentile")));
  for (const vtab of ["generate_series", "stats_seq"]) assert.ok(SQLEAN_FUNCTIONS.has(vtab));
});

const turso = skip || (!existsSync(join(DRIVERS, "@tursodatabase", "database")) && "lab/drivers has no @tursodatabase/database installed");

/** A project on a file Turso's own engine made, with a full-text index unless `fts` is false, and defaults that call its functions. */
async function tursoProject(drivers: string[], fts = true): Promise<{ root: string; env: Record<string, string> }> {
  const root = mkdtempSync(join(tmpdir(), "reylo-turso-"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ dependencies: Object.fromEntries(drivers.map((d) => [d, "*"])) }));
  for (const d of drivers) link(root, d);
  const db = await tursoConnect(join(root, "app.db"));
  await db.exec("create table docs (id integer primary key, title text, body text not null)");
  if (fts) await db.exec("create index docs_fts on docs using fts (title, body)");
  await db.exec("create table users (id integer primary key, name text not null, public_id text not null unique default (uuid4_str()), stamp blob not null default (time_now()))");
  await db.exec("insert into docs (title, body) values ('kiwi', 'apple pie'), (null, 'kiwi and banana')");
  await db.exec("insert into users (name) values ('Ada')");
  await db.close();
  return { root, env: { DATABASE_URL: "file:./app.db" } };
}

type TursoDb = { exec(sql: string): Promise<void>; prepare(sql: string): { all(...params: unknown[]): Promise<Record<string, unknown>[]> }; close(): Promise<void> };
async function tursoConnect(path: string): Promise<TursoDb> {
  const { connect } = (await import(pathToFileURL(join(DRIVERS, "@tursodatabase", "database", "dist", "promise.js")).href)) as { connect(path: string, options?: object): Promise<TursoDb> };
  return connect(path, { experimental: ["index_method"] });
}

// Each of Turso's functions the engine knows, over the rows of `docs` and `users`, with the
// parameters it takes. Every value Turso's engine answers must be of the type the type check infers.
const TURSO_CALLS: [string, Record<string, unknown>?][] = [
  ["uuid4()"], ["uuid7()"], ["uuid4_str()"], ["uuid7_str()"], ["uuid7_str(:at)", { at: 1700000000 }], ["uuid7(:at)", { at: 1700000000 }], ["gen_random_uuid()"],
  ["fts_match(title, body, :q)", { q: "kiwi" }], ["fts_score(title, body, :q)", { q: "kiwi" }], ["fts_highlight(body, :q, '<b>', '</b>')", { q: "kiwi" }], ["fts_highlight(title, 'kiwi', '<b>', '</b>')"],
  ["vector32_sparse(:v)", { v: "[1,0,2]" }], ["vector_concat(vector(:v), vector(:w))", { v: "[1,2]", w: "[3]" }], ["vector_slice(vector(:v), :from, 2)", { v: "[1,2,3]", from: 0 }],
  ["vector_distance_dot(vector(:v), vector(:w))", { v: "[1,2]", w: "[3,4]" }], ["vector_distance_jaccard(vector(:v), vector(:w))", { v: "[1,2]", w: "[3,4]" }],
  ["time_now()"], ["time_date(2024, 1, 2, 3, 4, 5)"], ["time_get(time_now(), :field)", { field: "second" }], ["time_get(time_now(), :field)", { field: "year" }], ["time_get(time_now(), 'second')"], ["time_get(time_now(), 'epoch')"], ["time_get(time_now(), 'year')"],
  ...["year", "month", "day", "hour", "minute", "second", "nano", "weekday", "yearday", "isoyear", "isoweek"].map((f) => [`time_get_${f}(time_now())`] as [string]),
  ["time_unix(:s)", { s: 1700000000 }], ["time_milli(1700000000000)"], ["time_micro(1)"], ["time_nano(1)"],
  ...["unix", "milli", "micro", "nano"].map((u) => [`time_to_${u}(time_unix(1700000000))`] as [string]),
  ...["after", "before", "compare", "equal", "sub"].map((f) => [`time_${f}(time_now(), time_unix(1700000000))`] as [string]),
  ["time_add(time_now(), dur_h())"], ["time_add_date(time_now(), 1, 2, 3)"], ["time_since(time_unix(1700000000))"], ["time_until(time_unix(1700000000))"],
  ["time_trunc(time_now(), 'day')"], ["time_round(time_now(), dur_h())"], ["time_parse(:text)", { text: "2024-01-02T03:04:05Z" }],
  ...["iso", "datetime", "date", "time"].map((f) => [`time_fmt_${f}(time_now(), :offset)`, { offset: 3600 }] as [string, Record<string, unknown>]),
  ...["ns", "us", "ms", "s", "m", "h"].map((u) => [`dur_${u}()`] as [string]),
];

/** The storage classes a value of a type of the descriptor may have, as SQLite's `typeof` names them. */
const STORED: Record<string, string[]> = { integer: ["integer"], boolean: ["integer"], float: ["real"], text: ["text"], bytes: ["blob"], union: ["integer", "real"] };

test("a file Turso's own engine made is read, full-text index and all, and its functions are typed as it answers them", { skip: turso, timeout: 60_000 }, async () => {
  const { root, env } = await tursoProject(["@tursodatabase/database"]);
  const session = await Session.open(root, env);
  const db = await tursoConnect(join(root, "app.db"));
  try {
    const typed = async (sql: string) => {
      const outcome = await session.analyze(sql);
      assert.ok(outcome.ok, !outcome.ok ? outcome.message : "");
      assert.deepEqual(outcome.analysis.diagnostics, [], sql);
      return outcome.analysis;
    };
    await typed("select id, body from docs where id = :id");
    // `match` prepares on a column the full-text index covers, and an insert runs the defaults.
    await typed("select id from docs where body match :q");
    await typed("insert into users (name) values (:name) returning id, public_id, stamp");
    for (const [call, params = {}] of TURSO_CALLS) {
      const column = (await typed(`select ${call} as v from docs`)).columns[0]!;
      const stored = await db.prepare(`select typeof(${call}) as t from docs`).all(params);
      for (const { t } of stored) {
        const allowed = [...(STORED[column.type.kind] ?? [String(t)]), ...(column.nullable ? ["null"] : [])];
        assert.ok(allowed.includes(String(t)), `${call}: typed ${column.type.kind}${column.nullable ? " or null" : ""}, Turso answers ${t}`);
      }
    }
    // `uuid4()` and `uuid7()` are bytes on Turso's own engine.
    for (const call of ["uuid4()", "uuid7()"]) assert.equal((await typed(`select ${call} as v`)).columns[0]!.type.kind, "bytes");
  } finally {
    await db.close();
    await session.close();
  }
  // Every function of Turso's the engine knows is held to Turso's answers above.
  const walker = readFileSync(new URL("../engine/sqlite/csrc/reylo_sqlite.c", import.meta.url), "utf8");
  const table = walker.slice(walker.indexOf("/* Turso's own engine"), walker.indexOf("};", walker.indexOf("aTurso[] = {")));
  const known = new Set([...table.matchAll(/\{"([a-z0-9_]+)",/g)].map((m) => m[1]!));
  assert.ok(known.size > 40, `${known.size}`);
  assert.deepEqual([...known].filter((name) => !TURSO_CALLS.some(([call]) => call.includes(`${name}(`))), []);
});

test("a project that runs libSQL beside Turso's own engine types uuid4() as either's value", { skip: turso, timeout: 60_000 }, async () => {
  // libSQL's SQLite can't read a file with a full-text index, and a driver that can't open the file is left out.
  const { root, env } = await tursoProject(["@tursodatabase/database", "@libsql/client"], false);
  const outcome = await analyze(root, env, "select uuid4() as v");
  assert.equal(outcome.analysis.columns[0]!.type.kind, "unknown");
});
