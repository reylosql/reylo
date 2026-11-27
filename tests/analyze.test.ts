import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { connect, createServer, type AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { Session } from "../src/analyze.ts";
import { engine, type Connection } from "../src/engine.ts";
import { sqlModule } from "../src/loader.ts";
import { mapperHandlers } from "../src/mapper-server.ts";
import { reportedDiagnostics, type Reported } from "../src/mapper-diagnostics.ts";
import { mysqlStandIn } from "./mysql-stand-in.ts";
import { refusedLogin, Unreachable } from "../src/connection.ts";
import { defineQuery } from "../src/index.ts";
import { nativeBinary } from "../src/native.ts";
import { DRIVERS, PG, clickhouseDatabase, clickhouseLive, mysqlDatabase, mysqlLive, postgresDatabase } from "./helpers.ts";

// A throwaway Postgres server for the tests that need one, never a database anyone uses: each such
// test makes a database of its own there and drops it after. The driver is the labs', never the
// package's own.
const live = !PG ? "REYLO_TEST_PG names no server" : !existsSync(join(DRIVERS, "postgres")) ? "lab/drivers is not installed" : false;

// A SQLite project: a package, its database, and DATABASE_URL naming it.
function project(): { root: string; env: Record<string, string> } {
  const root = mkdtempSync(join(tmpdir(), "reylo-analyze-"));
  writeFileSync(join(root, "package.json"), "{}");
  const db = new DatabaseSync(join(root, "app.db"));
  db.exec("create table users (id integer primary key, name text not null)");
  db.close();
  return { root, env: { DATABASE_URL: "file:./app.db" } };
}

// Each session stands for a process of its own: the type check, a build.
async function analyze(root: string, env: Record<string, string>, sql: string, options: { force?: boolean } = {}) {
  const session = await Session.open(root, env);
  try {
    return await session.analyze(sql, options);
  } finally {
    await session.close();
  }
}

const SELECT = "select id, name from users where id = :id";

// A project file's command runs in the system's shell, cmd.exe on Windows, and these tests write theirs for a POSIX shell.
const posixShell = process.platform === "win32" && "its command is written for a POSIX shell, and Windows runs it in cmd.exe";

test("a statement is analyzed against the database once, and read from reylo/.cache/ by every process after", async () => {
  const { root, env } = project();
  const first = await analyze(root, env, SELECT);
  assert.ok(first.ok);
  assert.equal(first.source, "database");
  assert.equal(first.analysis.cardinality, "at_most_one");
  assert.ok(first.entry.startsWith(join(root, "reylo", ".cache", "queries")));
  assert.ok(existsSync(first.entry));
  // The directory keeps itself out of git.
  assert.match(readFileSync(join(root, "reylo", ".cache", ".gitignore"), "utf8"), /^\*$/m);
  const second = await analyze(root, env, SELECT);
  assert.ok(second.ok);
  assert.deepEqual([second.source, second.entry, second.analysis], ["cache", first.entry, first.analysis]);
});

test("a change to the schema makes the entry stale, and the statement is analyzed again", async () => {
  const { root, env } = project();
  const before = await analyze(root, env, "select * from users");
  assert.ok(before.ok);
  assert.deepEqual(before.analysis.columns.map((c) => c.name), ["id", "name"]);
  const db = new DatabaseSync(join(root, "app.db"));
  db.exec("alter table users add column email text");
  db.close();
  const after = await analyze(root, env, "select * from users");
  assert.ok(after.ok);
  assert.equal(after.source, "database");
  assert.deepEqual(after.analysis.columns.map((c) => c.name), ["id", "name", "email"]);
  assert.equal(after.entry, before.entry);
});

test("a session's identity, which the type check gives TypeScript, moves with the schema and the app's SQLite drivers, and is its own with no URL", async () => {
  const { root, env } = project();
  writeFileSync(join(root, "package.json"), JSON.stringify({ dependencies: { "better-sqlite3": "*" } }));
  // A driver that resolves and cannot open the database: it is left out of the analysis, and its version is still part of what the statements were checked with.
  const driver = join(root, "node_modules", "better-sqlite3");
  mkdirSync(driver, { recursive: true });
  writeFileSync(join(driver, "index.js"), "throw new Error('no native module');\n");
  const installed = (version: string) => writeFileSync(join(driver, "package.json"), JSON.stringify({ name: "better-sqlite3", version, main: "index.js" }));
  const identity = async (at: Record<string, string> = env) => {
    const session = await Session.open(root, at);
    try {
      return await session.identity();
    } finally {
      await session.close();
    }
  };
  installed("12.0.0");
  const first = await identity();
  assert.equal(await identity(), first);
  installed("12.1.0");
  const upgraded = await identity();
  assert.notEqual(upgraded, first);
  const db = new DatabaseSync(join(root, "app.db"));
  db.exec("alter table users add column email text");
  db.close();
  assert.notEqual(await identity(), upgraded);
  // With no URL, the analyses come from reylo/.cache/, and no two sessions share an identity.
  const unset = await identity({});
  assert.match(unset, /"offline \d+ \d+"/);
  await new Promise((resolve) => setTimeout(resolve, 2));
  assert.notEqual(await identity({}), unset);
});

test("a database with the same schema at another URL reads the entry, and one with another schema writes its own", async () => {
  const { root, env } = project();
  const first = await analyze(root, env, SELECT);
  assert.ok(first.ok);
  assert.equal(first.source, "database");
  // A copy of the file, as a fresh database built by the same migrations is.
  cpSync(join(root, "app.db"), join(root, "fresh.db"));
  const fresh = await analyze(root, { DATABASE_URL: "file:./fresh.db" }, SELECT);
  assert.ok(fresh.ok);
  assert.deepEqual([fresh.source, fresh.entry], ["cache", first.entry]);
  const db = new DatabaseSync(join(root, "fresh.db"));
  db.exec("alter table users add column email text");
  db.close();
  const changed = await analyze(root, { DATABASE_URL: "file:./fresh.db" }, SELECT);
  assert.ok(changed.ok);
  assert.equal(changed.source, "database");
  assert.notEqual(changed.entry, first.entry);
});

test("a statement keeps the entries of a few databases, and the newest of them", async () => {
  const { root, env } = project();
  await analyze(root, env, SELECT);
  for (let i = 0; i < 6; i++) {
    cpSync(join(root, "app.db"), join(root, `build${i}.db`));
    const db = new DatabaseSync(join(root, `build${i}.db`));
    db.exec(`create table t${i} (id integer)`);
    db.close();
    const built = await analyze(root, { DATABASE_URL: `file:./build${i}.db` }, SELECT);
    assert.ok(built.ok);
    assert.equal(built.source, "database");
    assert.ok(existsSync(built.entry));
  }
  assert.equal(readdirSync(join(root, "reylo", ".cache", "queries")).length, 4);
});

test("a session that still trusts its fingerprint takes another process's newer entry, and never writes the old schema back", async () => {
  const { root, env } = project();
  const sql = "select id from users where name = :name";
  // A build step's session, kept open, which read the fingerprint moments ago.
  const build = await Session.open(root, env);
  try {
    const before = await build.analyze(sql);
    assert.ok(before.ok);
    assert.equal(before.analysis.cardinality, "many");
    const db = new DatabaseSync(join(root, "app.db"));
    db.exec("create unique index users_name on users (name)");
    db.close();
    // The type check, a process of its own, analyzes the statement on the new schema.
    const checked = await analyze(root, env, sql);
    assert.ok(checked.ok);
    assert.deepEqual([checked.source, checked.analysis.cardinality], ["database", "at_most_one"]);
    const after = await build.analyze(sql);
    assert.ok(after.ok);
    assert.deepEqual([after.source, after.analysis.cardinality], ["cache", "at_most_one"]);
    assert.equal((JSON.parse(readFileSync(after.entry, "utf8")) as { analysis: { cardinality: string } }).analysis.cardinality, "at_most_one");
  } finally {
    await build.close();
  }
});

// A migration, then its revert, on a database the session has read the fingerprint of; and a second
// database built by the same migrations, which has not run it.
async function migratedUnderASession(open: (root: string, env: Record<string, string>) => Promise<Session>, a: { url: string; run: (sql: string) => Promise<unknown> }, b: { url: string }, migrate: string, revert: string) {
  const root = mkdtempSync(join(tmpdir(), "reylo-migrated-"));
  writeFileSync(join(root, "package.json"), "{}");
  const session = await open(root, { DATABASE_URL: a.url });
  try {
    assert.ok((await session.analyze("select b from u")).ok);
    // A build step's session closes its connection when it idles, and still trusts its fingerprint.
    await new Promise((resolve) => setTimeout(resolve, 50));
    await a.run(migrate);
    const during = await session.analyze("select a from t");
    assert.ok(during.ok);
    assert.equal(during.source, "database");
    assert.ok(!during.analysis.columns[0]!.nullable);
    // Written, under the migrated schema's fingerprint.
    assert.equal((JSON.parse(readFileSync(during.entry, "utf8")) as { schema: string }).schema, (await session.schema()).value);
  } finally {
    await session.close();
  }
  // Neither the database that has not run the migration nor the one that reverted it is served its analysis.
  const unmigrated = await analyze(root, { DATABASE_URL: b.url }, "select a from t");
  assert.ok(unmigrated.ok);
  assert.ok(unmigrated.analysis.columns[0]!.nullable);
  await a.run(revert);
  const reverted = await analyze(root, { DATABASE_URL: a.url }, "select a from t");
  assert.ok(reverted.ok);
  assert.ok(reverted.analysis.columns[0]!.nullable);
}

test("an analysis made after a migration, by a session that still trusts the fingerprint from before it, is written under the migrated schema's", async () => {
  const dir = mkdtempSync(join(tmpdir(), "reylo-migrated-db-"));
  // SQLite changes a column by building its table again; t is made so too, so that the revert gives back the same schema.
  const rebuild = (a: string) => `create table t2 (a ${a}); insert into t2 select a from t; drop table t; alter table t2 rename to t;`;
  const database = (name: string) => {
    const exec = (sql: string) => {
      const db = new DatabaseSync(join(dir, name));
      db.exec(sql);
      db.close();
    };
    exec("create table u (b int); create table t2 (a int); alter table t2 rename to t;");
    return { url: `file:${join(dir, name)}`, run: async (sql: string) => exec(sql) };
  };
  // A session whose connection closes after 10 ms idle, as a build step's does after its last file.
  await migratedUnderASession((root, env) => Session.open(root, env, 10), database("a.db"), database("b.db"), rebuild("int not null"), rebuild("int"));
});

test("on Postgres, an analysis made after a migration, by a session that still trusts the fingerprint from before it, is written under the migrated schema's", { skip: live }, async () => {
  const setup = "create table u (b int); create table t (a int);";
  const a = await postgresDatabase("reylo_migrated_a", setup);
  const b = await postgresDatabase("reylo_migrated_b", setup);
  try {
    // The type check's session, which keeps its connection.
    await migratedUnderASession((root, env) => Session.open(root, env), { url: a.url, run: (sql) => a.sql.unsafe(sql) }, b, "alter table t alter a set not null", "alter table t alter a drop not null");
  } finally {
    await a.drop();
    await b.drop();
  }
});

test("on MySQL, an analysis made after a migration, by a session that still trusts the fingerprint from before it, is written under the migrated schema's", { skip: mysqlLive }, async () => {
  const setup = "create table u (b int); create table t (a int)";
  const a = await mysqlDatabase("reylo_migrated_a", setup);
  const b = await mysqlDatabase("reylo_migrated_b", setup);
  try {
    await migratedUnderASession((root, env) => Session.open(root, env), { url: a.url, run: (sql) => a.db.query(sql) }, b, "alter table t modify a int not null", "alter table t modify a int null");
  } finally {
    await a.drop();
    await b.drop();
  }
});

test("on ClickHouse, an analysis made after a migration, by a session that still trusts the fingerprint from before it, is written under the migrated schema's", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const setup = "CREATE TABLE u (b Int32) ENGINE = MergeTree ORDER BY tuple();\nCREATE TABLE t (a Nullable(Int32)) ENGINE = MergeTree ORDER BY tuple()";
  const a = await clickhouseDatabase("reylo_migrated_a", setup);
  const b = await clickhouseDatabase("reylo_migrated_b", setup);
  try {
    await migratedUnderASession((root, env) => Session.open(root, env), { url: a.url, run: (query) => a.client.command({ query }) }, b, "ALTER TABLE t MODIFY COLUMN a Int32 DEFAULT 0", "ALTER TABLE t MODIFY COLUMN a Nullable(Int32)");
  } finally {
    await a.drop();
    await b.drop();
  }
});

/**
 * Runs `run` with each connection the engine of `dialect` opens changed by `change`, as a test
 * stands in for what happens on the database while a session works.
 */
async function withConnections<T>(dialect: "sqlite", change: (c: Connection) => Connection, run: () => Promise<T>): Promise<T> {
  const e = await engine(dialect);
  const open = e.open;
  e.open = async (url) => change(await open.call(e, url));
  try {
    return await run();
  } finally {
    e.open = open;
  }
}

test("on SQLite, a schema changed and changed back while a statement is analyzed leaves no entry of the schema in between", async () => {
  const { root, env } = project();
  const exec = (sql: string) => {
    const db = new DatabaseSync(join(root, "app.db"));
    db.exec(sql);
    db.close();
  };
  // SQLite changes a column by building its table again; users is made so for the revert too.
  const rebuild = (name: string) => `create table users2 (id integer primary key, name ${name}); insert into users2 select * from users; drop table users; alter table users2 rename to users;`;
  exec(rebuild("text not null"));
  let analyses = 0;
  const outcome = await withConnections(
    "sqlite",
    (c) => ({
      ...c,
      // The first analysis runs on a migration, which is reverted before it ends: the fingerprint
      // after it is the one before it, and only the schema's stamp says it moved.
      analyze: async (sql) => {
        if (analyses++ > 0) return c.analyze(sql);
        exec(rebuild("text"));
        try {
          return await c.analyze(sql);
        } finally {
          exec(rebuild("text not null"));
        }
      },
    }),
    () => analyze(root, env, "select name from users"),
  );
  assert.ok(outcome.ok);
  assert.equal(analyses, 2);
  assert.ok(!outcome.analysis.columns[0]!.nullable);
  assert.ok(!(JSON.parse(readFileSync(outcome.entry, "utf8")) as { analysis: { columns: { nullable?: boolean }[] } }).analysis.columns[0]!.nullable);
});

test("the engine's answers from while the fingerprint query failed are dropped once it runs again", async () => {
  const root = mkdtempSync(join(tmpdir(), "reylo-unverified-"));
  writeFileSync(join(root, "package.json"), "{}");
  const exec = (sql: string) => {
    const db = new DatabaseSync(join(root, "app.db"));
    db.exec(sql);
    db.close();
  };
  exec("create table t (a int)");
  let refused = true;
  await withConnections(
    "sqlite",
    // A fingerprint the database refuses: the session's fingerprint is its own, unverified value.
    (c) => ({ ...c, schema: () => (refused ? Promise.reject(new Error("no such table: reylo_no_such_table")) : c.schema()) }),
    async () => {
      const session = await Session.open(root, { DATABASE_URL: "file:./app.db" });
      try {
        const before = await session.analyze("select * from t");
        assert.ok(before.ok);
        assert.deepEqual(before.analysis.columns.map((c) => c.name), ["a"]);
        refused = false;
        exec("alter table t add column b int");
        await session.recheck();
        const after = await session.analyze("select * from t");
        assert.ok(after.ok);
        assert.deepEqual(after.analysis.columns.map((c) => c.name), ["a", "b"]);
      } finally {
        await session.close();
      }
    },
  );
});

test("without the database, reylo/.cache/ answers as it is, and a statement it lacks is an error that says where to fill it", async () => {
  const { root, env } = project();
  assert.ok((await analyze(root, env, SELECT)).ok);
  renameSync(join(root, "app.db"), join(root, "gone.db"));
  const cached = await analyze(root, env, SELECT);
  assert.ok(cached.ok);
  assert.equal(cached.source, "cache");
  // Nothing held the entry to the database, which the outcome says, with why and the entry's day.
  assert.match(cached.unverified ?? "", /^cannot open the database .*app\.db \(DATABASE_URL in the environment\).*; building from reylo\/\.cache\/, analyzed \d{4}-\d\d-\d\d$/);
  const missing = await analyze(root, env, "select name from users");
  assert.ok(!missing.ok);
  assert.equal(missing.code, 5);
  assert.match(missing.message, /^Cannot open the database .*app\.db .*reylo\/\.cache has no analysis of this statement, so run the type check or a build where the database can be reached first\.$/);
  // --force asks the database even for a statement reylo/.cache/ holds.
  const forced = await analyze(root, env, SELECT, { force: true });
  assert.ok(!forced.ok);
});

test("two databases the project uses in turn keep an entry each, so neither analyzes the other's statements again", async () => {
  const { root, env } = project();
  const db = new DatabaseSync(join(root, "test.db"));
  db.exec("create table users (id integer primary key, name text not null, email text)");
  db.close();
  const test = { DATABASE_URL: "file:./test.db" };
  const sources = [];
  for (const e of [env, test, env, test]) {
    const outcome = await analyze(root, e, "select * from users");
    assert.ok(outcome.ok);
    sources.push(`${outcome.source} ${outcome.analysis.columns.length}`);
  }
  assert.deepEqual(sources, ["database 2", "database 3", "cache 2", "cache 3"]);
  // Where neither can be reached, a URL with no entry of its own reads the one written last, by
  // the time it records, which a copy that resets the files' times (cp -r, fs.cpSync) keeps.
  for (const f of ["app.db", "test.db"]) renameSync(join(root, f), join(root, `${f}.gone`));
  const queries = join(root, "reylo", ".cache", "queries");
  const [older, newer] = readdirSync(queries).map((n) => join(queries, n)).sort((a, b) => JSON.parse(readFileSync(a, "utf8")).at - JSON.parse(readFileSync(b, "utf8")).at);
  utimesSync(newer!, new Date(2000, 0, 1), new Date(2000, 0, 1));
  utimesSync(older!, new Date(), new Date());
  const offline = await analyze(root, { DATABASE_URL: "file:./elsewhere.db" }, "select * from users");
  assert.ok(offline.ok);
  assert.deepEqual([offline.source, offline.entry, offline.analysis.columns.length], ["cache", newer, 3]);
});

test("a build that cannot reach the database reads reylo/.cache/ with another URL, with none, and in a copy of the project elsewhere", async () => {
  const { root, env } = project();
  // The entries record their dialect, which names the database where no URL does.
  const typed = await analyze(root, env, SELECT);
  assert.ok(typed.ok);
  assert.equal(typed.source, "database");
  // No URL: the entries are used as they are, and a statement they lack needs a URL.
  const bare = await analyze(root, {}, SELECT);
  assert.ok(bare.ok);
  assert.deepEqual([bare.source, bare.entry], ["cache", typed.entry]);
  assert.match(bare.unverified ?? "", /^no database URL is set; building from reylo\/\.cache\/, analyzed \d{4}-\d\d-\d\d$/);
  assert.equal(typed.unverified, undefined);
  const lacking = await analyze(root, {}, "select name from users");
  assert.ok(!lacking.ok);
  assert.deepEqual([lacking.code, lacking.message], [4, 'No database URL: set DATABASE_URL in the environment, .env.local or .env, or "direct_url" in .reylo.yaml.']);
  assert.ok(!(await analyze(root, {}, SELECT, { force: true })).ok);
  // Another URL, to a database the build cannot open, reads the same entry.
  const other = await analyze(root, { DATABASE_URL: "file:./elsewhere.db" }, SELECT);
  assert.ok(other.ok);
  assert.deepEqual([other.source, other.entry], ["cache", typed.entry]);
  // A copy of the project at another path, as a container build has it, without its database.
  const copy = mkdtempSync(join(tmpdir(), "reylo-analyze-copy-"));
  cpSync(root, copy, { recursive: true, verbatimSymlinks: true });
  rmSync(join(copy, "app.db"));
  const moved = await analyze(copy, env, SELECT);
  assert.ok(moved.ok);
  assert.equal(moved.source, "cache");
  assert.ok(moved.entry.startsWith(join(copy, "reylo", ".cache")));
});

test("on a server, a URL that fails reads that server's own entry and no other's, and a build says once that it reads reylo/.cache/ unchecked", { skip: live }, async (t) => {
  const { url, sql, drop } = await postgresDatabase("reylo_offline", "create table users (id int primary key, email text not null)");
  const root = mkdtempSync(join(tmpdir(), "reylo-offline-"));
  writeFileSync(join(root, "package.json"), "{}");
  const statement = "select id, email from users where id = :id";
  // The server at an address of its own, which can stop answering while the test's server goes on.
  const target = new URL(url);
  const proxy = createServer((socket) => {
    const up = connect(Number(target.port), target.hostname);
    socket.pipe(up).pipe(socket);
    socket.on("error", () => up.destroy());
    up.on("error", () => socket.destroy());
  });
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  const through = new URL(url);
  through.hostname = "127.0.0.1";
  through.port = String((proxy.address() as AddressInfo).port);
  const env = { DATABASE_URL: through.href };
  try {
    const typed = await analyze(root, env, statement);
    assert.ok(typed.ok);
    assert.deepEqual([typed.source, typed.unverified], ["database", undefined]);
    await sql.unsafe("alter table users alter email drop not null");
    await new Promise((resolve) => proxy.close(resolve));
    // The server no longer answers: its own entry, as old as it is, and the outcome says so.
    const stale = await analyze(root, env, statement);
    assert.ok(stale.ok);
    assert.deepEqual([stale.source, stale.entry], ["cache", typed.entry]);
    assert.match(stale.unverified ?? "", /^cannot connect to the database at 127\.0\.0\.1:\d+\/reylo_offline_\d+ \(DATABASE_URL in the environment\): .+; building from reylo\/\.cache\/, analyzed \d{4}-\d\d-\d\d$/);
    // A server nothing was analyzed on reads no other's entry: a mistyped host is not the database last analyzed.
    const elsewhere = new URL(url);
    elsewhere.port = "1";
    const other = await analyze(root, { DATABASE_URL: elsewhere.href }, statement);
    assert.ok(!other.ok);
    assert.match(other.message, /^Cannot connect to the database at .*:1\/reylo_offline_\d+ .*; reylo\/\.cache has this statement's analysis from another database only, which a build reads where no database URL is set: run the type check or a build where this database can be reached, or build with no URL\.$/);
    // With no URL, the newest entry of any server answers, and says that nothing checked it.
    const bare = await analyze(root, {}, statement);
    assert.ok(bare.ok);
    assert.deepEqual([bare.source, bare.entry], ["cache", typed.entry]);
    assert.match(bare.unverified ?? "", /^no database URL is set; building from reylo\/\.cache\/, analyzed \d{4}-\d\d-\d\d$/);
    // A build step prints the line once a run, however many modules it builds, and the type check's hover carries it.
    const warned: string[] = [];
    t.mock.method(console, "warn", (line: string) => void warned.push(line));
    writeFileSync(join(root, ".env"), `DATABASE_URL=${through.href}\n`);
    for (const name of ["a.sql", "b.sql"]) writeFileSync(join(root, name), statement);
    const mapper = mapperHandlers();
    const projectHandle = `offline-${root}`;
    await mapper.openProject!({ projectHandle, configFileName: join(root, "tsconfig.json") });
    try {
      await sqlModule(join(root, "a.sql"), statement);
      await sqlModule(join(root, "b.sql"), statement);
      const declared = (await mapper.transform!({ fileName: join(root, "a.sql"), content: statement, projectHandle })) as { text: string } & Reported;
      assert.deepEqual(reportedDiagnostics(declared), []);
      assert.match(declared.text, /\n \* reylo: cannot connect to the database at 127\.0\.0\.1:\d+\/reylo_offline_\d+ \(DATABASE_URL in \.env\): .+; building from reylo\/\.cache\/, analyzed \d{4}-\d\d-\d\d\n \*\/\ndeclare const query: /);
    } finally {
      await mapper.closeProject!({ projectHandle });
      for (const session of Session.opened()) await session.close();
    }
    assert.equal(warned.length, 1);
    assert.match(warned[0]!, /^reylo: cannot connect to the database at 127\.0\.0\.1:\d+\/reylo_offline_\d+ \(DATABASE_URL in \.env\): .+; building from reylo\/\.cache\/, analyzed \d{4}-\d\d-\d\d$/);
  } finally {
    proxy.close();
    await drop();
  }
});

test("Postgres: the DateStyle error is no part of another style's entry, so it goes when the database's style is ISO again", { skip: live }, async () => {
  const { url, sql, drop } = await postgresDatabase("reylo_datestyle_cache", "create table orders (id int primary key, ships_on date not null)");
  const root = mkdtempSync(join(tmpdir(), "reylo-datestyle-"));
  writeFileSync(join(root, "package.json"), "{}");
  const database = new URL(url).pathname.slice(1);
  const statement = "select id, ships_on from orders";
  // Each call is a session of its own, whose connection starts with the database's style of the moment.
  const checked = async (at = url) => {
    const outcome = await analyze(root, { DATABASE_URL: at }, statement);
    assert.ok(outcome.ok);
    return [outcome.source, outcome.analysis.diagnostics.map((d) => d.code)];
  };
  try {
    assert.deepEqual(await checked(), ["database", []]);
    await sql.unsafe(`alter database ${database} set datestyle = 'SQL, DMY'`);
    assert.deepEqual(await checked(), ["database", [24]]);
    assert.deepEqual(await checked(), ["cache", [24]]);
    // The URL's own setting is the session's: the entry made under the other style is not its answer.
    assert.deepEqual(await checked(`${url}?options=-c%20datestyle%3DISO`), ["database", []]);
    // The order of a date's fields (ISO, DMY and ISO, MDY) is no part of how one is written.
    await sql.unsafe(`alter database ${database} reset datestyle`);
    assert.deepEqual(await checked(), ["cache", []]);
  } finally {
    await drop();
  }
});

test("Postgres: under another DateStyle, reylo generate rust-sqlx and the type check write one entry, and each reads the other's as its own", { skip: live }, async () => {
  const { url, sql, drop } = await postgresDatabase("reylo_datestyle_rust", "create table orders (id int primary key, ships_on date not null)");
  const statement = "select id, ships_on from orders where id = :id\n";
  // A crate whose reylo reads dates as chrono's, and a package, in one directory, which share reylo/.cache/.
  const crate = () => {
    const root = mkdtempSync(join(tmpdir(), "reylo-datestyle-rust-"));
    writeFileSync(join(root, ".reylo.yaml"), `direct_url: ${JSON.stringify(url)}\n`);
    writeFileSync(join(root, "package.json"), "{}");
    writeFileSync(join(root, "Cargo.toml"), '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nreylo = { version = "0.9", features = ["chrono"] }\n');
    mkdirSync(join(root, "src"));
    writeFileSync(join(root, "src", "lib.rs"), "");
    writeFileSync(join(root, "q.sql"), statement);
    return root;
  };
  const generated = (root: string) => {
    const run = spawnSync(nativeBinary(), ["generate", "rust-sqlx", "q.sql", "--out", "queries.rs"], { cwd: root, encoding: "utf8" });
    assert.equal(run.status, 0, run.stderr + run.stdout);
    return readFileSync(join(root, "queries.rs"), "utf8");
  };
  const checked = async (root: string) => {
    const outcome = await analyze(root, {}, statement);
    assert.ok(outcome.ok);
    return [outcome.source, outcome.analysis.diagnostics.map((d) => d.code)];
  };
  // The cache's one entry as it stands: its file's name and its whole text, `at` included, which a
  // writer that analysed the statement again would change.
  const entry = (root: string) => {
    const queries = join(root, "reylo", ".cache", "queries");
    const files = readdirSync(queries);
    assert.equal(files.length, 1, files.join(", "));
    return { file: files[0]!, text: readFileSync(join(queries, files[0]!), "utf8") };
  };
  const analysis = (written: { text: string }) => JSON.parse(written.text).analysis;
  try {
    await sql.unsafe(`alter database ${new URL(url).pathname.slice(1)} set datestyle = 'SQL, DMY'`);
    // The generator first: it writes the module, since sqlx reads a date in the binary format, which
    // no DateStyle changes, and the type check reads its entry with the error of its own, as .run()
    // reads a date's text.
    const first = crate();
    const module = generated(first);
    assert.match(module, /pub ships_on: [^,\n]*\bNaiveDate\b/);
    const generators = entry(first);
    assert.deepEqual(await checked(first), ["cache", [24]]);
    assert.deepEqual(entry(first), generators);
    // The type check first: the generator reads its entry, and writes the module it writes with none.
    const second = crate();
    assert.deepEqual(await checked(second), ["database", [24]]);
    const checks = entry(second);
    assert.deepEqual(analysis(checks), analysis(generators));
    assert.equal(generated(second), module);
    assert.deepEqual(entry(second), checks);
  } finally {
    await drop();
  }
});

test("the module a build step emits carries the analysis and reads nothing at run time", async () => {
  const { root, env } = project();
  const file = join(root, "user.sql");
  writeFileSync(file, SELECT);
  // The build steps share one session per project; this one finds the database through .env.
  writeFileSync(join(root, ".env"), `DATABASE_URL=${env.DATABASE_URL}\n`);
  try {
    const { code, entry, watched } = await sqlModule(file, SELECT);
    assert.ok(existsSync(entry));
    // A bundler watches the files the module's text reads besides the statement: here the .env
    // file, the only one of the project's files that exists.
    assert.deepEqual(watched, [join(root, ".env")]);
    assert.equal(code, [
      `import { defineSqliteQuery } from "reylo";`,
      `export const sql = "select id, name from users where id = ?";`,
      `export const raw = ${JSON.stringify(SELECT)};`,
      `export const params = ["id"];`,
      `export default defineSqliteQuery({ sql: raw, params, cardinality: "maybeOne", readOnly: true, text: sql, order: ["id"], inputs: {"id":{"type":{"kind":"integer","bits":64},"nullable":false}}, columns: [["id","int"],["name","text"]], row: (v) => ({ "id": v[0], "name": v[1] }), source: "user.sql", sqlcommenter: true, jsonSchema: {"input":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"id":{"type":"integer","format":"int64"}},"required":["id"],"additionalProperties":false},"output":{"$schema":"https://json-schema.org/draft/2020-12/schema","anyOf":[{"type":"object","properties":{"id":{"type":"integer","format":"int64"},"name":{"type":"string"}},"required":["id","name"]},{"type":"null"}]}} });`,
      ``,
    ].join("\n"));
    // A statement in a directory carries its file written with `/`.
    mkdirSync(join(root, "queries"));
    const one = join(root, "queries", "one.sql");
    writeFileSync(one, "select 1 as n");
    const { code: plain } = await sqlModule(one, "select 1 as n");
    assert.ok(plain.includes(`, source: "queries/one.sql", `));
    // A build (a process of its own) that has neither the database nor the entry fails instead of guessing.
    for (const session of Session.opened()) await session.close();
    renameSync(join(root, "app.db"), join(root, "gone.db"));
    await assert.rejects(sqlModule(file, "select name from users"), /user\.sql: Cannot open the database .*run the type check or a build where/);
  } finally {
    for (const session of Session.opened()) await session.close();
  }
});

test("one statement of another kind is a command: DDL resolves to nothing and runs, and a PRAGMA returns its rows", async () => {
  const { root, env } = project();
  const ddl = "create index users_name on users (name)";
  const created = await analyze(root, env, ddl);
  assert.ok(created.ok);
  const { kind, cardinality, columns, params, diagnostics } = created.analysis;
  assert.deepEqual({ kind, cardinality, columns, params, diagnostics }, { kind: "other", cardinality: "none", columns: [], params: [], diagnostics: [] });
  const db = new DatabaseSync(join(root, "app.db"));
  try {
    assert.equal(await defineQuery({ dialect: "sqlite", sql: ddl, params: [], cardinality: cardinality === "at_most_one" ? "maybeOne" : cardinality }).run(db), undefined);
    // A command that returns rows has the columns SQLite names, of no known type and nullable.
    const listed = await analyze(root, env, "pragma index_list(users)");
    assert.ok(listed.ok);
    assert.equal(listed.analysis.cardinality, "many");
    assert.deepEqual(listed.analysis.columns.map((c) => [c.name, c.type.kind, c.nullable]), ["seq", "name", "unique", "origin", "partial"].map((name) => [name, "unknown", true]));
    const rows = await defineQuery({ dialect: "sqlite", sql: "pragma index_list(users)", params: [], cardinality: "many" }).run(db);
    assert.deepEqual(rows.map((r) => (r as { name: string }).name), ["users_name"]);
  } finally {
    db.close();
  }
  // A CREATE of a table the database has is SQLite's error, with how to leave a migration out.
  const existing = await analyze(root, env, "create table users (id integer primary key)");
  assert.ok(existing.ok);
  assert.deepEqual(existing.analysis.diagnostics.map((d) => [d.code, d.message]), [[6, 'table users already exists, so if it is a migration or a schema file, leave it out of the type check with "exclude" in tsconfig.json, as in "exclude": ["db/**"].']]);
});

test("a build fails on a statement the database refuses, at its line and column as the type check places it", async () => {
  const { root, env } = project();
  writeFileSync(join(root, ".env"), `DATABASE_URL=${env.DATABASE_URL}\n`);
  try {
    // The offset is in UTF-8 bytes; the column counts characters, past the two-byte é.
    const sql = "select 'é' as e,\n  nmae from users";
    const file = join(root, "bad.sql");
    writeFileSync(file, sql);
    await assert.rejects(sqlModule(file, sql), /bad\.sql:2:3: no such column: nmae$/);
  } finally {
    for (const session of Session.opened()) await session.close();
  }
});

test("a parameter written twice is an error at each later use, which the type check places and a build fails on; a with reads the value twice", async () => {
  const { root, env } = project();
  writeFileSync(join(root, ".env"), `DATABASE_URL=${env.DATABASE_URL}\n`);
  const twice = "select id, name from users\nwhere name like :q or name = :q or id = :q";
  try {
    const session = await Session.for(root);
    const outcome = await session.analyze(twice);
    assert.ok(outcome.ok);
    // Byte offsets, as the content mapper hands them to the type check: the second and third :q.
    const second = twice.indexOf(":q or id");
    assert.deepEqual(outcome.analysis.diagnostics.map((d) => [d.code, d.start, d.length]), [[3, second, 2], [3, twice.lastIndexOf(":q"), 2]]);
    assert.equal(outcome.analysis.diagnostics[0]!.message, "The parameter :q appears more than once; use each parameter once, and select it in a with to read it twice, cast to its type, as in: with input as (select :q::<type> as q) select … from input");
    const file = join(root, "search.sql");
    writeFileSync(file, twice);
    await assert.rejects(sqlModule(file, twice), /search\.sql: The parameter :q appears more than once/);
    // SQLite's own forms and its numbered placeholder are names too; a bare ? never repeats.
    for (const sql of ["select id from users where name = @q or name = @q", "select id from users where id = ?1 or id = ?1", "select id from users where id = ? or id = ?1"]) {
      const refused = await session.analyze(sql);
      assert.ok(refused.ok);
      assert.deepEqual(refused.analysis.diagnostics.map((d) => d.code), [3], sql);
    }
    const bare = await session.analyze("select id from users where id = ? or id = ?");
    assert.ok(bare.ok);
    assert.deepEqual(bare.analysis.diagnostics, []);
    // Cast to nullable(…) in the with, the parameter may be null, and a coalesce of its column reads
    // that null.
    const once = "with input as (select :q::nullable(text) as q) select id, name from users, input where name like coalesce(input.q, '%') or name = input.q";
    const read = await session.analyze(once);
    assert.ok(read.ok);
    assert.deepEqual(read.analysis.diagnostics, []);
    assert.deepEqual(read.analysis.params.map((p) => [p.name, p.nullable]), [["q", true]]);
    writeFileSync(file, once);
    const { code } = await sqlModule(file, once);
    assert.match(code, /export const params = \["q"\];/);
  } finally {
    for (const session of Session.opened()) await session.close();
  }
});

test("a parameter as a whole sort key is an error the type check places, and a build still runs the statement it typed", async () => {
  const { root, env } = project();
  writeFileSync(join(root, ".env"), `DATABASE_URL=${env.DATABASE_URL}\n`);
  const sql = "select id, name from users\norder by :sort";
  try {
    const session = await Session.for(root);
    const outcome = await session.analyze(sql);
    assert.ok(outcome.ok);
    assert.deepEqual(outcome.analysis.diagnostics.map((d) => [d.code, d.start, d.length]), [[17, sql.indexOf(":sort"), 5]]);
    assert.deepEqual(outcome.analysis.columns.map((c) => c.name), ["id", "name"]);
    // As an ON CONFLICT target no key matches, the error is the type check's: the module is built.
    const file = join(root, "sorted.sql");
    writeFileSync(file, sql);
    const { code } = await sqlModule(file, sql);
    assert.match(code, /export const params = \["sort"\];/);
  } finally {
    for (const session of Session.opened()) await session.close();
  }
});

test("a Postgres database built by the same migrations reads the entries of another, whatever its object ids", { skip: live }, async () => {
  const migrations = `
    create type mood as enum ('calm', 'loud');
    create domain short as text check (length(value) < 9);
    create table people (id serial primary key, mood mood not null, nick short, tags text[], born timestamptz default now());
    create index on people (mood);
    create function touch() returns trigger language plpgsql as $$ begin return new; end $$;
    create trigger touch before update on people for each row execute function touch();
    create function adults() returns setof people language sql stable as 'select * from people';
    create view moods as select id, mood from people;
    create policy everyone on people using (true);
    comment on column people.nick is 'What they go by';
  `;
  const first = await postgresDatabase("reylo_same_first", migrations);
  // Object ids are given out across a whole server, so that anything created first moves them.
  const second = await postgresDatabase("reylo_same_second", `create table moved (id int); drop table moved; ${migrations}`);
  const root = mkdtempSync(join(tmpdir(), "reylo-fingerprint-"));
  writeFileSync(join(root, "package.json"), "{}");
  const sql = "select id, mood, nick from people where id = :id";
  try {
    const typed = await analyze(root, { DATABASE_URL: first.url }, sql);
    assert.ok(typed.ok);
    assert.equal(typed.source, "database");
    const fresh = await analyze(root, { DATABASE_URL: second.url }, sql);
    assert.ok(fresh.ok);
    assert.deepEqual([fresh.source, fresh.entry], ["cache", typed.entry]);
    assert.deepEqual(fresh.analysis, typed.analysis);
    // What the analysis said of the objects is the second database's to say too.
    await second.sql.unsafe("alter table people alter column nick set not null");
    const changed = await analyze(root, { DATABASE_URL: second.url }, sql);
    assert.ok(changed.ok);
    assert.equal(changed.source, "database");
  } finally {
    await first.drop();
    await second.drop();
  }
});

test("the Postgres fingerprint runs on the server, holds while nothing changes, and moves with each catalog the analysis reads, under a search path that puts a user's `||` first", { skip: live }, async () => {
  const own = await postgresDatabase("reylo_fingerprint");
  const url = new URL(own.url);
  const db = own.sql;
  const root = mkdtempSync(join(tmpdir(), "reylo-fingerprint-"));
  writeFileSync(join(root, "package.json"), "{}");
  mkdirSync(join(root, "node_modules"));
  symlinkSync(join(DRIVERS, "postgres"), join(root, "node_modules", "postgres"));
  // A session per call, as a process of its own, so that none answers from a fingerprint it took.
  const fingerprint = async () => {
    const session = await Session.open(root, { DATABASE_URL: url.href });
    try {
      return (await session.schema()).value;
    } finally {
      await session.close();
    }
  };
  // A base type of text's functions, with `options` beside them.
  const baseType = (options: string) =>
    `create type mt; create function mt_in(cstring) returns mt language internal immutable strict as 'textin'; create function mt_out(mt) returns cstring language internal immutable strict as 'textout'; create type mt (input = mt_in, output = mt_out, internallength = variable, storage = extended, ${options})`;
  // An aggregate that runs `step` and `done`, but for the options `swap` names.
  const total = (swap: Record<string, string> = {}) => {
    const options = { sfunc: "step", finalfunc: "done", combinefunc: "step", msfunc: "step", minvfunc: "step", mfinalfunc: "done", ...swap };
    return `create or replace aggregate total(int) (stype = int, mstype = int, ${Object.entries(options).map(([option, name]) => `${option} = ${name}`).join(", ")})`;
  };
  try {
    await db.unsafe(`create table a (x int, s text); create table b (x int, s text); create table m (v varchar(20), n numeric);
      create function skip() returns trigger language plpgsql as 'begin return null; end';
      create function to_one(boolean) returns int8 language sql as 'select 1::int8';
      create function plus(int, int) returns int language sql as 'select $1 + $2';
      create function step(int, int) returns int language sql as 'select $1 + $2';
      create function other_step(int, int) returns int language sql as 'select $1 + $2';
      create function done(int) returns int language sql as 'select $1';
      create function other_done(int) returns int language sql as 'select $1';
      ${total()}`);
    // A query the server refuses gives "unverified <pid> <time>", a value no entry ever matches.
    const first = await fingerprint();
    assert.match(first ?? "", /^[0-9a-f]{16}$/);
    assert.equal(await fingerprint(), first);
    // A user's `||` that answers NULL, which a search path naming pg_catalog after public puts
    // before the built-in: the query builds its text with pg_catalog's own, so every row still
    // reads, and the fingerprint moves with each change below.
    await db.unsafe(`create function cat_null(text, text) returns text language sql immutable as 'select null::pg_catalog.text';
      create operator public.|| (leftarg = text, rightarg = text, function = cat_null)`);
    await db.unsafe(`alter database ${url.pathname.slice(1)} set search_path = public, pg_catalog`);
    const hostile = await fingerprint();
    assert.notEqual(hostile, first);
    assert.equal(await fingerprint(), hostile);
    // Each change touches only a catalog the row count or the trust check reads: rows of b are read
    // by a scan of a, a BEFORE ROW trigger can skip an INSERT's row and a rule can replace it, a
    // key holds only under the index's collation, and a user's operator, cast, default argument,
    // volatility or strictness decides which call the server makes and what it may answer, and a
    // column's type modifier is the size, precision or scale its type carries.
    let last = hostile;
    for (const change of [
      "alter table b inherit a",
      "create trigger skip before insert on a for each row execute function skip()",
      "alter table a disable trigger skip",
      "create rule nothing as on insert to b do instead nothing",
      `alter table a alter column s type text collate "C"`,
      "create operator === (leftarg = int, rightarg = int, function = int4eq)",
      "create cast (boolean as int8) with function to_one(boolean)",
      "drop cast (boolean as int8); create cast (boolean as int8) with function to_one(boolean) as implicit",
      "create or replace function plus(int, int default 1) returns int language sql as 'select $1 + $2'",
      "alter function plus(int, int) immutable",
      "alter function plus(int, int) strict",
      // What a body returns, which an analysis reads: a body stored parsed keeps prosrc empty.
      "create or replace function plus(int, int default 2) returns int language sql strict immutable as 'select $1 + $2'",
      "alter function plus(int, int) set search_path = public",
      "create function atomic_plus(a int) returns int language sql begin atomic select a + 1; end",
      "create or replace function atomic_plus(a int) returns int language sql begin atomic select a + 2; end",
      "alter table m alter column v type varchar(50)",
      "alter table m alter column n type numeric(10,2)",
      // A function's result columns and its arguments' names, which a call reads and its body may not.
      "create function people() returns table (id int, name text) language sql as 'select 1, ''a''::text'",
      "drop function people(); create function people() returns table (id int, full_name text) language sql as 'select 1, ''a''::text'",
      "create function pair(out a int, out b int) language sql as 'select 1, 2'",
      "drop function pair(); create function pair(out a bigint, out b int) language sql as 'select 1, 2'",
      "create function named(x int) returns int language sql as 'select $1'",
      "drop function named(int); create function named(y int) returns int language sql as 'select $1'",
      // The functions an aggregate runs, whose volatility and language say whether a call may
      // write: each option in turn names another function, then its own again, and one function's
      // language changes alone, as the server allows with its bodies unchecked.
      ...["sfunc", "combinefunc", "msfunc", "minvfunc"].flatMap((option) => [total({ [option]: "other_step" }), total()]),
      ...["finalfunc", "mfinalfunc"].flatMap((option) => [total({ [option]: "other_done" }), total()]),
      "begin; set local check_function_bodies = off; create or replace function done(int) returns int language plpgsql as 'select $1'; commit",
      // An extension's aggregates are read as a user's wherever it is created, pg_catalog included:
      // citext's `min` runs `citext_larger`, then its own `citext_smaller` again.
      "create extension citext schema pg_catalog",
      ...["citext_larger", "citext_smaller"].map((fn) => `create or replace aggregate pg_catalog.min(citext) (sfunc = ${fn}, combinefunc = ${fn}, stype = citext)`),
      // Whether an INSERT may give an identity column a value.
      "alter table m add column id int generated by default as identity",
      "alter table m alter column id set generated always",
      // Whether a collation is deterministic, which a key's equality and a CHECK's narrowing read:
      // one dropped and made again under its name keeps every column that names it as it was.
      "create collation folded (provider = icu, locale = 'und', deterministic = true)",
      "drop collation folded; create collation folded (provider = icu, locale = 'und', deterministic = false)",
      // A function's language, which decides whether its body is read, and whether it runs as its
      // owner or has a planner support function, either of which keeps its body unread.
      "create function g() returns int language sql as 'select 1'",
      "begin; set local check_function_bodies = off; create or replace function g() returns int language plpgsql as 'select 1'; commit",
      "alter function g() security definer",
      "alter function g() support pg_catalog.varchar_support",
      // A function of an extension's in pg_catalog, read wherever it was made.
      "alter function pg_catalog.citext_eq(citext, citext) volatile",
      "alter function pg_catalog.citext_eq(citext, citext) called on null input",
      // Whether a backslash in a string is an escape, which the CHECKs are read under, and whether
      // one may escape a quote: a new session takes the database's setting.
      `alter database ${url.pathname.slice(1)} set standard_conforming_strings = off`,
      `alter database ${url.pathname.slice(1)} reset standard_conforming_strings`,
      `alter database ${url.pathname.slice(1)} set backslash_quote = off`,
      // An operator family's equality, which decides whether a unique key pins one row.
      "create operator family fam using btree; alter operator family fam using btree add operator 3 = (int4, int4)",
      "alter operator family fam using btree drop operator 3 (int4, int4)",
      // The tables an extension owns, which its code may read.
      "create table side (a int)",
      "alter extension citext add table side",
      "alter extension citext drop table side",
      // A sequence a column owns, which an insert may advance.
      "create sequence extra",
      "alter sequence extra owned by a.x",
      "alter sequence extra owned by none",
      // A domain's collation, and a base type's category, preference and array delimiter, which
      // none of its functions carries: each made again under its own name.
      `create domain d as text collate "C"`,
      `drop domain d; create domain d as text collate "POSIX"`,
      baseType("delimiter = ',', category = 'U'"),
      `drop type mt cascade; ${baseType("delimiter = ';', category = 'S', preferred = true")}`,
    ]) {
      await db.unsafe(change);
      const next = await fingerprint();
      assert.notEqual(next, last, change);
      assert.equal(await fingerprint(), next, change);
      last = next;
    }
    // A temporary aggregate is one session's, which another session's statement names only by that
    // session's number, as it names a temporary function: the fingerprint each new session reads
    // holds while the test's own connection makes one and after it drops it.
    for (const change of ["create aggregate pg_temp.review_total(int) (sfunc = pg_catalog.int4pl, stype = int, initcond = '0')", "drop aggregate pg_temp.review_total(int)"]) {
      await db.unsafe(change);
      assert.equal(await fingerprint(), last, change);
    }
  } finally {
    await own.drop();
  }
});

test("the MySQL fingerprint holds while nothing changes, and moves with a column's length, precision and scale, fractional seconds and character set, which its type carries, with a CHECK's expression and whether it is enforced, with a stored function's arguments, and with what another database holds, but for Reylo Migrate's shadows other than the URL's own, and with the account's TRIGGER privileges", { skip: mysqlLive }, async () => {
  const root = mkdtempSync(join(tmpdir(), "reylo-fingerprint-"));
  writeFileSync(join(root, "package.json"), "{}");
  mkdirSync(join(root, "node_modules"));
  symlinkSync(join(DRIVERS, "mysql2"), join(root, "node_modules", "mysql2"));
  const own = await mysqlDatabase(
    "reylo_fingerprint",
    `create table m (v varchar(20), n decimal(10,2), at datetime(3), body text character set latin1);
    create table docs (j json not null, constraint j_shape check (json_schema_valid('{"type":"object","properties":{"a":{"type":"integer"}}}', j)));
    create function f(x int) returns int deterministic return x`,
  );
  const ownName = new URL(own.url).pathname.slice(1);
  const shadow = `reylo_shadow_fingerprint_${process.pid}`;
  // The fingerprint reads every database the account may reach, so the session's account is granted
  // only this test's: the databases other tests make and drop meanwhile are out of its sight.
  const user = `reylo_fp_${process.pid}`;
  // A session per call, as a process of its own, so that none answers from a fingerprint it took.
  const fingerprint = async (database = ownName) => {
    const url = new URL(own.url);
    url.username = user;
    url.password = "";
    url.pathname = `/${database}`;
    const session = await Session.open(root, { DATABASE_URL: url.href });
    try {
      return (await session.schema()).value;
    } finally {
      await session.close();
    }
  };
  let other: Awaited<ReturnType<typeof mysqlDatabase>> | undefined;
  try {
    other = await mysqlDatabase("reylo_fingerprint_other", "create table t (c int not null); create function g(x int) returns int deterministic return x");
    const otherName = new URL(other.url).pathname.slice(1);
    await own.db.query(`drop user if exists '${user}'@'%'`);
    await own.db.query(`create user '${user}'@'%'`);
    // A `_` in a granted database's name matches any character, unless escaped.
    const granted = (database: string) => `\`${database.replaceAll("_", "\\_")}\`.*`;
    for (const database of [ownName, otherName, shadow]) await own.db.query(`grant all on ${granted(database)} to '${user}'@'%'`);
    let last = await fingerprint();
    assert.match(last ?? "", /^[0-9a-f]{16}$/);
    assert.equal(await fingerprint(), last);
    // A TEXT's character set decides whether its size counts the bytes a client receives.
    // A JSON column is typed by the schema its CHECK holds it to, while the CHECK is enforced.
    const checks = [`alter table docs drop check j_shape, add constraint j_shape check (json_schema_valid('{"type":"object","properties":{"a":{"type":"string"}}}', j))`, "alter table docs alter check j_shape not enforced"];
    // A parameter is typed as the argument it is passed for, and a call checked against their count.
    const signatures = ["f(x varchar(10))", "f(x varchar(10), y int)"].map((signature) => ["drop function f", `create function ${signature} returns int deterministic return x`]);
    // A structured parameter is expanded by the database's character set and collation.
    const database = [`alter database ${ownName} character set latin1 collate latin1_swedish_ci`, `alter database ${ownName} collate latin1_bin`];
    // Which keys a structured parameter's rows may leave out is read from each column's default.
    const defaults = ["alter table m alter v set default 'x'", "alter table m alter v drop default"];
    // A statement may read another database's table or call its function, and the engine takes a
    // column of a table for a view's wherever any database has a view of that name.
    const elsewhere = [`alter table ${otherName}.t modify c varchar(10) null`, `create view ${otherName}.m as select 1 as v`, [`drop function ${otherName}.g`, `create function ${otherName}.g(x varchar(10)) returns int deterministic return x`]];
    // Without the TRIGGER privilege, a written table's triggers are hidden from the account. A grant
    // on a database's pattern names the pattern, which the engine does not read as the database.
    // And a statement is prepared against the account's privileges, a column's included.
    const privileges = [`grant trigger on \`${ownName}\`.m to '${user}'@'%'`, `grant select (v) on \`${ownName}\`.m to '${user}'@'%'`];
    for (const change of ["alter table m modify v varchar(50)", "alter table m modify n decimal(12,4)", "alter table m modify at datetime(6)", "alter table m modify body text character set utf8mb4", ...checks, ...signatures, ...elsewhere, ...privileges, ...database, ...defaults]) {
      const statements = [change].flat();
      for (const statement of statements) await own.db.query(statement);
      const next = await fingerprint();
      assert.notEqual(next, last, statements.join("; "));
      assert.equal(await fingerprint(), next, statements.join("; "));
      last = next;
    }
    // A shadow database, which each check of the migrations makes and drops, under the lock that
    // keeps another check from dropping it as a leftover.
    await own.db.query(`select get_lock('${shadow}', 0)`);
    await own.db.query(`create database ${shadow}`);
    await own.db.query(`create table ${shadow}.t (c int)`);
    assert.equal(await fingerprint(), last);
    // But a dry run's type check reads a shadow as its own database.
    const before = await fingerprint(shadow);
    await own.db.query(`alter table ${shadow}.t modify c varchar(10)`);
    assert.notEqual(await fingerprint(shadow), before);
  } finally {
    try {
      await own.db.query(`drop database if exists ${shadow}`);
      await own.db.query(`select release_lock('${shadow}')`);
      await own.db.query(`drop user if exists '${user}'@'%'`);
    } finally {
      try {
        await other?.drop();
      } finally {
        await own.drop();
      }
    }
  }
});

test("on MySQL, a view in another check's shadow leaves the analysis of a table of the same name as it was", { skip: mysqlLive }, async () => {
  const root = mkdtempSync(join(tmpdir(), "reylo-shadow-view-"));
  writeFileSync(join(root, "package.json"), "{}");
  mkdirSync(join(root, "node_modules"));
  symlinkSync(join(DRIVERS, "mysql2"), join(root, "node_modules", "mysql2"));
  const own = await mysqlDatabase("reylo_view_base", "create table base (n int not null)");
  const shadow = `reylo_shadow_view_${process.pid}`;
  // A fresh analysis in a session of its own, which holds no answer from before.
  const columns = async () => {
    const session = await Session.open(root, { DATABASE_URL: own.url });
    try {
      const outcome = await session.analyze("select n from base", { force: true, store: false });
      assert.ok(outcome.ok);
      return JSON.stringify(outcome.analysis.columns);
    } finally {
      await session.close();
    }
  };
  try {
    const before = await columns();
    // Under the lock that keeps another check from dropping it as a leftover.
    await own.db.query(`select get_lock('${shadow}', 0)`);
    await own.db.query(`create database ${shadow}`);
    await own.db.query(`create view ${shadow}.base as select 1 as n`);
    assert.equal(await columns(), before);
  } finally {
    try {
      await own.db.query(`drop database if exists ${shadow}`);
      await own.db.query(`select release_lock('${shadow}')`);
    } finally {
      await own.drop();
    }
  }
});

// A project on MySQL whose server is a stand-in (tests/mysql-stand-in.ts), named in .env so that a
// build step, which reads the process's environment, finds it too.
function standInProject(port: number): string {
  const root = mkdtempSync(join(tmpdir(), "reylo-not-mysql-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, ".env"), `DATABASE_URL=mysql://root@127.0.0.1:${port}/app\n`);
  return root;
}

test("a MariaDB server is refused by the type check, once per connection, with an error that says to type-check against MySQL", async () => {
  const server = await mysqlStandIn({ version: "11.8.2-MariaDB-ubu2404" });
  const root = standInProject(server.port);
  const session = await Session.open(root, {});
  try {
    for (const sql of ["select 1 as one", "select 2 as two"]) {
      const outcome = await session.analyze(sql);
      assert.ok(!outcome.ok);
      assert.equal(outcome.code, 5);
      assert.equal(
        outcome.message,
        `Cannot type-check against the database at 127.0.0.1:${server.port}/app (DATABASE_URL in .env): it is MariaDB 11.8.2, and Reylo type-checks MySQL statements against MySQL. Point the type check's URL at a MySQL server, 8.4 or later, with the same schema.`,
      );
    }
    // One connection asked once, then closed; nothing else ran on it.
    assert.deepEqual(server.queries, ["select version()"]);
    assert.ok(server.ended());
    assert.ok((await session.schema()).fatal);
  } finally {
    await session.close();
  }
  // A build step, which reads the process's environment, fails at the file with the same error.
  const saved = process.env.DATABASE_URL;
  delete process.env.DATABASE_URL;
  try {
    await assert.rejects(sqlModule(join(root, "q.sql"), "select 1"), /q\.sql: Cannot type-check against .* it is MariaDB 11\.8\.2/);
  } finally {
    if (saved !== undefined) process.env.DATABASE_URL = saved;
    for (const session of Session.opened()) await session.close();
    await server.close();
  }
});

test("a MySQL server is not refused: the type check goes on to read its schema", async () => {
  const server = await mysqlStandIn({ version: "8.4.6" });
  const session = await Session.open(standInProject(server.port), {});
  try {
    const schema = await session.schema();
    assert.ok(!schema.fatal);
    assert.match(schema.value ?? "", /^[0-9a-f]{16}$/);
    // The version, the bound on waiting for a table's lock, then the schema's fingerprint.
    assert.equal(server.queries[0], "select version()");
    assert.equal(server.queries[1], "set session lock_wait_timeout = cast(least(@@session.lock_wait_timeout, 10) as unsigned)");
    assert.equal(server.queries.length, 3);
  } finally {
    await session.close();
    await server.close();
  }
});

// A MySQL stand-in that takes one password, and refuses any other at the login as MySQL does (error 1045).
const TOKENS = ["token-1", "token-2", "token-3"];

test("a login the server refuses runs direct_url's command again, once, so that an expired token is replaced", async () => {
  let current = "token-1";
  const server = await mysqlStandIn({ version: "8.4.6", accepts: () => current, passwords: TOKENS });
  const root = mkdtempSync(join(tmpdir(), "reylo-token-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, "token.txt"), "token-1");
  writeFileSync(join(root, ".reylo.yaml"), `direct_url: mysql://root:$(cat token.txt)@127.0.0.1:${server.port}/app\n`);
  const session = await Session.open(root, {});
  try {
    // The token the session read at its start has expired, and the command now prints the next.
    current = "token-2";
    writeFileSync(join(root, "token.txt"), "token-2");
    const schema = await session.schema();
    assert.equal(schema.error, undefined);
    assert.match(schema.value ?? "", /^[0-9a-f]{16}$/);
    assert.deepEqual(server.tried, ["token-1", "token-2"]);
    // A command whose fresh output is refused too is asked once, and the refusal is the error.
    current = "token-3";
    await session.release();
    assert.ok(await session.recheck());
    const refused = await session.schema();
    assert.equal(refused.value, null);
    assert.match(refused.error ?? "", /Access denied for user 'root'/);
    assert.deepEqual(server.tried, ["token-1", "token-2", "token-2"]);
  } finally {
    await session.close();
    await server.close();
  }
});

// The same on Postgres against the live server, where a role that does not exist is a refused login
// (SQLSTATE 28000) as an expired password is.
test("on Postgres, a login the server refuses runs direct_url's command again, once", { skip: live }, async () => {
  const root = mkdtempSync(join(tmpdir(), "reylo-pg-token-"));
  writeFileSync(join(root, "package.json"), "{}");
  const server = new URL(PG!);
  writeFileSync(join(root, "role.txt"), "reylo_no_such_role");
  writeFileSync(join(root, ".reylo.yaml"), `direct_url: postgres://$(cat role.txt)@${server.host}${server.pathname}\n`);
  const session = await Session.open(root, {});
  try {
    // The role the session read at its start is refused, and the command now prints one that logs in.
    writeFileSync(join(root, "role.txt"), server.username || "postgres");
    const schema = await session.schema();
    assert.equal(schema.error, undefined);
    assert.match(schema.value ?? "", /^[0-9a-f]{16}$/);
    // The connection the fresh URL opened is the one the analysis runs on.
    const outcome = await session.analyze("select 1 as one", { force: true });
    assert.deepEqual(outcome.ok && outcome.analysis.columns.map((c) => c.name), ["one"]);
  } finally {
    await session.close();
  }
});

test("calls refused together when a token expires share one run of the command and one new connection", { skip: posixShell }, async () => {
  let current = "token-1";
  const server = await mysqlStandIn({ version: "8.4.6", accepts: () => current, passwords: TOKENS });
  const root = mkdtempSync(join(tmpdir(), "reylo-token-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, "token.txt"), "token-1");
  writeFileSync(join(root, ".reylo.yaml"), `direct_url: mysql://root:$(echo run >> runs.txt; cat token.txt)@127.0.0.1:${server.port}/app\n`);
  const runs = () => readFileSync(join(root, "runs.txt"), "utf8").split("\n").filter(Boolean).length;
  const session = await Session.open(root, {});
  try {
    assert.match((await session.schema()).value ?? "", /^[0-9a-f]{16}$/);
    const before = runs();
    // The token expires while the session's connection is closed, and two reads of the schema then
    // ask for one at once: the refused login runs the command once, for one new connection.
    current = "token-2";
    writeFileSync(join(root, "token.txt"), "token-2");
    await session.release();
    const [changed, schema] = await Promise.all([session.recheck(), session.schema()]);
    assert.equal(changed, false);
    assert.match(schema.value ?? "", /^[0-9a-f]{16}$/);
    assert.equal(runs() - before, 1);
    assert.deepEqual(server.tried, ["token-1", "token-1", "token-2"]);
    assert.equal(server.open(), 1);
  } finally {
    await session.close();
  }
  // The engine's goodbye reaches the server before the session's close returns.
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(server.open(), 0);
  await server.close();
});

test("a session kept open runs a failed direct_url command again once its failure has stood a minute", { skip: posixShell }, async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() });
  const server = await mysqlStandIn({ version: "8.4.6", accepts: () => "token-1", passwords: TOKENS });
  const root = mkdtempSync(join(tmpdir(), "reylo-token-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, ".reylo.yaml"), `direct_url: mysql://root:$(echo run >> runs.txt; cat token.txt)@127.0.0.1:${server.port}/app\n`);
  const runs = () => readFileSync(join(root, "runs.txt"), "utf8").split("\n").filter(Boolean).length;
  // The password manager is locked when the session opens, as the type check's may be at the editor's start.
  const session = await Session.open(root, {});
  try {
    const failed = await session.analyze("select 1");
    assert.equal(failed.ok, false);
    assert.equal(!failed.ok && failed.code, 11);
    assert.match(!failed.ok ? failed.message : "", /runs `echo run >> runs.txt; cat token.txt`, which exited with code 1/);
    // Unlocked, the command is not run again within the minute, so that a prompt is not asked again per file.
    writeFileSync(join(root, "token.txt"), "token-1");
    const soon = await session.analyze("select 1");
    assert.equal(!soon.ok && soon.code, 11);
    assert.equal(runs(), 1);
    // After it, the next analysis runs the command, and the session logs in with what it prints.
    t.mock.timers.tick(60_000);
    const later = await session.analyze("select 1");
    // The stand-in prepares nothing, so the analysis itself still fails, past the project.
    assert.equal(!later.ok && later.code, 5);
    assert.equal(session.project.error, undefined);
    assert.equal(runs(), 2);
    // The connection the stand-in drops at the prepare is opened once more for the same analysis.
    assert.deepEqual(server.tried, ["token-1", "token-1"]);
  } finally {
    await session.close();
    await server.close();
  }
});

test("the type check names the dialect of the project a kept session recovers to", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() });
  const server = await mysqlStandIn({ version: "8.4.6", accepts: () => "token-1", passwords: TOKENS });
  const root = mkdtempSync(join(tmpdir(), "reylo-token-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, ".reylo.yaml"), `direct_url: mysql://root:$(cat token.txt)@127.0.0.1:${server.port}/app\n`);
  const mapper = mapperHandlers();
  const projectHandle = `recovers-${root}`;
  await mapper.openProject!({ projectHandle, configFileName: join(root, ".reylo.yaml") });
  const transform = async () => {
    const result = (await mapper.transform!({ fileName: join(root, "q.sql"), content: "select 1", projectHandle })) as { text: string } & Reported;
    return { text: result.text, diagnostics: reportedDiagnostics(result) };
  };
  try {
    // With no URL, and no entry in reylo/.cache/ to name one, the dialect is Postgres's.
    const before = await transform();
    assert.deepEqual(before.diagnostics.map((d) => d.code), [11]);
    assert.match(before.text, /QueryError<"postgres">/);
    writeFileSync(join(root, "token.txt"), "token-1");
    t.mock.timers.tick(60_000);
    // The stand-in prepares nothing, so the analysis fails past the project, in MySQL's terms.
    const after = await transform();
    assert.deepEqual(after.diagnostics.map((d) => d.code), [5]);
    assert.match(after.text, /QueryError<"mysql">/);
    assert.match(after.text, /"many", "mysql", QueryError>/);
  } finally {
    await mapper.closeProject!({ projectHandle });
    await server.close();
  }
});

test("a refused login is told apart from a server that cannot be reached, on Postgres and MySQL", () => {
  const pg = (code: string) => new Unreachable(Object.assign(new Error("password authentication failed"), { code, severity: "FATAL" }));
  assert.ok(refusedLogin(pg("28P01")));
  assert.ok(refusedLogin(pg("28000")));
  assert.ok(refusedLogin(new Unreachable(Object.assign(new Error("Access denied"), { code: "ER_ACCESS_DENIED_ERROR", errno: 1045 }))));
  assert.ok(!refusedLogin(pg("3D000")));
  assert.ok(!refusedLogin(new Unreachable(Object.assign(new Error("connect ECONNREFUSED"), { code: "ECONNREFUSED" }))));
});
