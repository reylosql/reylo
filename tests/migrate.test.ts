import { test } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { brokenKeys, migrate, runMigrations, runMigrationsOn, type MigrationConnection, type MigrationStorage } from "../src/migrate.ts";
import { driver } from "./helpers.ts";

// A Durable Object's storage over node:sqlite, as far as `migrate` uses it: `sql.exec` runs one or
// more statements, its bindings going to the last, and `transactionSync` rolls back on a throw.
function storage(db = new DatabaseSync(":memory:")): MigrationStorage & { db: DatabaseSync } {
  return {
    db,
    sql: {
      exec(query: string, ...bindings: unknown[]) {
        const statements = query.split(/;\s*(?=\S)/).filter((s) => s.trim());
        const last = statements.pop()!;
        for (const s of statements) db.exec(s);
        const prepared = db.prepare(last);
        const rows = prepared.columns().length ? prepared.all(...(bindings as [])) : (prepared.run(...(bindings as [])), []);
        return { toArray: () => rows };
      },
    },
    transactionSync<T>(closure: () => T): T {
      db.exec("begin");
      try {
        const out = closure();
        db.exec("commit");
        return out;
      } catch (e) {
        db.exec("rollback");
        throw e;
      }
    },
  };
}

const USERS = { name: "20261105120000_users.sql", sql: "create table users (id integer primary key, name text not null);" };
const BIO = { name: "20261106120000_bio.sql", sql: "alter table users add column bio text;" };
const columns = (s: { db: DatabaseSync }) => (s.db.prepare("select name from pragma_table_info('users')").all() as { name: string }[]).map((r) => r.name);

test("migrate runs each migration once, in order, and records it as reylo migrate does", () => {
  const s = storage();
  assert.deepEqual(runMigrations(s, [USERS]), [USERS.name]);
  s.db.exec("insert into users (name) values ('ada')");
  // The next deploy adds a migration: an object that ran the first runs only the second, and keeps its rows.
  assert.deepEqual(runMigrations(s, [USERS, BIO]), [BIO.name]);
  assert.deepEqual(columns(s), ["id", "name", "bio"]);
  assert.equal((s.db.prepare("select count(*) as n from users").get() as { n: number }).n, 1);
  assert.deepEqual(runMigrations(s, [USERS, BIO]), []);
  // The history table is reylo migrate's on SQLite: the file's name and SQL, and when it ran.
  const rows = s.db.prepare("select name, sql, applied_at is not null as applied, fingerprint, catalog from __reylo_migrations order by rowid").all();
  assert.deepEqual(rows.map((r) => ({ ...r })), [
    { name: USERS.name, sql: USERS.sql, applied: 1, fingerprint: null, catalog: null },
    { name: BIO.name, sql: BIO.sql, applied: 1, fingerprint: null, catalog: null },
  ]);
});

test("a migration that fails rolls back with its row, names itself, and leaves the ones before it run", () => {
  const s = storage();
  const broken = { name: "20261107120000_broken.sql", sql: "alter table users add column age int; alter table nowhere add column x int;" };
  assert.throws(() => runMigrations(s, [USERS, broken]), /^Error: reylo: the migration 20261107120000_broken\.sql failed on this Durable Object, and rolled back: no such table: nowhere/);
  assert.deepEqual(columns(s), ["id", "name"]);
  assert.deepEqual((s.db.prepare("select name from __reylo_migrations").all() as { name: string }[]).map((r) => r.name), [USERS.name]);
});

test("a migration that leaves a foreign key broken rolls back alone, as reylo migrate refuses it", () => {
  const s = storage();
  const POSTS = { name: "20261107120000_posts.sql", sql: "create table posts (id integer primary key, user_id integer references users (id) deferrable initially deferred); insert into users (name) values ('ada'); insert into posts (user_id) values (1);" };
  const ORPHANS = { name: "20261108120000_orphans.sql", sql: "delete from users;" };
  assert.deepEqual(runMigrations(s, [USERS, POSTS]), [USERS.name, POSTS.name]);
  assert.throws(() => runMigrations(s, [USERS, POSTS, ORPHANS]), /^Error: reylo: the migration 20261108120000_orphans\.sql failed on this Durable Object, and rolled back: 1 rows break a foreign key after this migration$/);
  assert.equal((s.db.prepare("select count(*) as n from users").get() as { n: number }).n, 1);
});

test("a rebuild that leaves every foreign key whole commits, though SQLite still counts its drop", () => {
  const s = storage();
  const POSTS = { name: "20261107120000_posts.sql", sql: "create table posts (id integer primary key, user_id integer references users (id) deferrable initially deferred); insert into users (name) values ('ada'); insert into posts (user_id) values (1);" };
  // The drop counts the post's key as broken, and the rename that makes it whole again leaves the count.
  const REBUILD = { name: "20261108120000_rebuild.sql", sql: "create table users_new (id integer primary key, name text not null check (length(name) > 0)); insert into users_new select * from users; drop table users; alter table users_new rename to users;" };
  assert.deepEqual(runMigrations(s, [USERS, POSTS, REBUILD]), [USERS.name, POSTS.name, REBUILD.name]);
  assert.equal((s.db.prepare("select count(*) as n from posts").get() as { n: number }).n, 1);
  assert.throws(() => s.db.exec("insert into users (id, name) values (2, '')"), /CHECK constraint failed/);
});

test("a history the bundle doesn't continue is refused before anything runs, naming the migration", () => {
  const AGE = { name: "20261108120000_age.sql", sql: "alter table users add column age int;" };
  const history = (s: { db: DatabaseSync }) => (s.db.prepare("select name from __reylo_migrations order by rowid").all() as { name: string }[]).map((r) => r.name);
  // A migration the object ran that the bundle lacks, as after a deploy rolled back past it.
  const s = storage();
  runMigrations(s, [USERS, BIO]);
  assert.throws(() => runMigrations(s, [USERS, AGE]), /^Error: reylo: this Durable Object ran the migration 20261106120000_bio\.sql, which this build doesn't have/);
  assert.deepEqual(history(s), [USERS.name, BIO.name]);
  assert.deepEqual(columns(s), ["id", "name", "bio"]);
  // One edited since the object ran it; trailing whitespace alone is no edit.
  assert.throws(() => runMigrations(s, [USERS, { ...BIO, sql: "alter table users add column bio text not null default '';" }, AGE]), /^Error: reylo: the migration 20261106120000_bio\.sql isn't the text this Durable Object ran/);
  assert.deepEqual(runMigrations(s, [USERS, { ...BIO, sql: BIO.sql + "\n\n" }]), []);
  // Nor, as the lock reads a migration, is a comment, spacing or a checkout's CRLF line endings.
  assert.deepEqual(runMigrations(s, [USERS, { ...BIO, sql: "-- the profile's text\r\nalter table users\r\n  add column bio text; /* nullable */\r\n" }]), []);
  // One the bundle's order puts before a migration the object already ran, as a branch merged late.
  const t = storage();
  runMigrations(t, [USERS, AGE]);
  assert.throws(() => runMigrations(t, [USERS, BIO, AGE]), /^Error: reylo: this Durable Object ran 20261108120000_age\.sql where this build's order has 20261106120000_bio\.sql first/);
  assert.deepEqual(columns(t), ["id", "name", "age"]);
});

test("migrate(ctx.storage) in a bundle Reylo's plugin didn't build says so, running nothing", () => {
  const s = storage();
  assert.throws(() => migrate(s), /^Error: reylo: this bundle holds no migrations for migrate: build it with reylo\/vite or reylo\/esbuild/);
  assert.equal(s.db.prepare("select count(*) as n from sqlite_schema").get()!.n, 0);
});

test("migrate() in a runtime that can't run the engine rejects, never throws", async () => {
  // As on a runtime with no `process.getBuiltinModule`, where the build-time half is out of reach.
  const host = process as { getBuiltinModule?: unknown };
  const saved = host.getBuiltinModule;
  host.getBuiltinModule = undefined;
  try {
    let promise: Promise<string[]> | undefined;
    assert.doesNotThrow(() => (promise = migrate()));
    await assert.rejects(promise!, /^Error: reylo cannot run its engine in this runtime/);
  } finally {
    host.getBuiltinModule = saved;
  }
});

// An app's own connection, as Turso's engine holds its file: the same steps as on a Durable Object,
// each migration between the connection's own begin and commit.
const names = async (db: MigrationConnection) => ((await (await (db.prepare("select name from __reylo_migrations order by rowid") as any)).all()) as { name: string }[]).map((r) => r.name);

test("migrate(db) runs each migration once on the app's own connection, on every SQLite client, and records it as reylo migrate does", async () => {
  const turso = await driver("@tursodatabase/database").connect(":memory:");
  const compat = new (driver("@tursodatabase/database/compat").Database)(":memory:");
  const better = new (driver("better-sqlite3"))(":memory:");
  for (const db of [turso, compat, better, new DatabaseSync(":memory:")] as MigrationConnection[]) {
    assert.deepEqual(await runMigrationsOn(db, [USERS]), [USERS.name]);
    await db.exec("insert into users (name) values ('ada')");
    assert.deepEqual(await runMigrationsOn(db, [USERS, BIO]), [BIO.name]);
    assert.deepEqual(await runMigrationsOn(db, [USERS, BIO]), []);
    assert.deepEqual(await names(db), [USERS.name, BIO.name]);
    const rows = (await (await (db.prepare("select name, bio from users") as any)).all()) as object[];
    assert.deepEqual(rows.map((r) => ({ ...r })), [{ name: "ada", bio: null }]);
  }
});

test("migrate(db): a migration that fails rolls back with its row and names itself, a broken foreign key included", async () => {
  const broken = { name: "20261107120000_broken.sql", sql: "alter table users add column age int; alter table nowhere add column x int;" };
  const POSTS = { name: "20261107120000_posts.sql", sql: "create table posts (id integer primary key, user_id integer references users (id) deferrable initially deferred); insert into users (name) values ('ada'); insert into posts (user_id) values (1);" };
  const ORPHANS = { name: "20261108120000_orphans.sql", sql: "delete from users;" };
  const turso = await driver("@tursodatabase/database").connect(":memory:");
  const better = new (driver("better-sqlite3"))(":memory:");
  for (const db of [turso, better, new DatabaseSync(":memory:")] as MigrationConnection[]) {
    await assert.rejects(runMigrationsOn(db, [USERS, broken]), /^Error: reylo: the migration 20261107120000_broken\.sql failed on this database, and rolled back: .*nowhere/);
    assert.deepEqual(await names(db), [USERS.name]);
    const columns = ((await (await (db.prepare("select name from pragma_table_info('users')") as any)).all()) as { name: string }[]).map((r) => r.name);
    assert.deepEqual(columns, ["id", "name"]);
    await db.exec("pragma foreign_keys = on");
    assert.deepEqual(await runMigrationsOn(db, [USERS, POSTS]), [POSTS.name]);
    // SQLite's foreign_key_check finds the orphan; on Turso's engine, whose check finds nothing before 0.9, each key is held from its declaration.
    await assert.rejects(runMigrationsOn(db, [USERS, POSTS, ORPHANS]), /^Error: reylo: the migration 20261108120000_orphans\.sql failed on this database, and rolled back: 1 rows break a foreign key after this migration$/);
    assert.deepEqual(await names(db), [USERS.name, POSTS.name]);
    const users = (await (await (db.prepare("select count(*) as n from users") as any)).all()) as { n: number }[];
    assert.equal(Number(users[0]!.n), 1);
    // The connection is out of the failed migration's transaction, takes the next, and has its foreign keys back on.
    assert.deepEqual(await runMigrationsOn(db, [USERS, POSTS, BIO]), [BIO.name]);
    const keys = (await (await (db.prepare("pragma foreign_keys") as any)).all()) as { foreign_keys: number }[];
    assert.equal(Number(keys[0]!.foreign_keys), 1);
  }
});

test("migrate(db) turns foreign keys off while a migration runs, so a rebuilt table's drop cascades to no row", async () => {
  const PARENT = { name: "20261107120000_posts.sql", sql: "create table posts (id integer primary key, user_id integer references users (id) on delete cascade); insert into users (name) values ('ada'); insert into posts (user_id) values (1);" };
  // SQLite's own recipe for a change alter table can't make: a new table, its rows copied, the old one dropped.
  const REBUILD = { name: "20261108120000_rebuild.sql", sql: "create table users_new (id integer primary key, name text not null check (length(name) > 0)); insert into users_new select * from users; drop table users; alter table users_new rename to users;" };
  const turso = await driver("@tursodatabase/database").connect(":memory:");
  for (const db of [turso, new (driver("better-sqlite3"))(":memory:"), new DatabaseSync(":memory:")] as MigrationConnection[]) {
    await db.exec("pragma foreign_keys = on");
    assert.deepEqual(await runMigrationsOn(db, [USERS, PARENT, REBUILD]), [USERS.name, PARENT.name, REBUILD.name]);
    const posts = (await (await (db.prepare("select count(*) as n from posts") as any)).all()) as { n: number }[];
    assert.equal(Number(posts[0]!.n), 1);
  }
});

test("migrate(db) leaves the app's foreign keys as it had them, whatever a migration sets", async () => {
  const OFF = { name: "20261107120000_off.sql", sql: "pragma foreign_keys = off; create table tags (id integer primary key);" };
  const turso = await driver("@tursodatabase/database").connect(":memory:");
  for (const db of [turso, new DatabaseSync(":memory:")] as MigrationConnection[]) {
    await db.exec("pragma foreign_keys = on");
    assert.deepEqual(await runMigrationsOn(db, [USERS, OFF]), [USERS.name, OFF.name]);
    const keys = (await (await (db.prepare("pragma foreign_keys") as any)).all()) as { foreign_keys: number }[];
    assert.equal(Number(keys[0]!.foreign_keys), 1);
  }
});

test("migrate(db) on Turso's engine holds keys from their declarations: composite, implied by the parent's primary key, and to a table that's gone", async () => {
  const db = await driver("@tursodatabase/database").connect(":memory:");
  const PAIRS = { name: "20261107120000_pairs.sql", sql: "create table pairs (a int, b int, primary key (a, b)); create table refs (x int, y int, note text, foreign key (x, y) references pairs); insert into pairs values (1, 2); insert into refs values (1, 2, 'ok'), (null, 9, 'unset'); create table \"odd \"\"name\" (id integer primary key);" };
  assert.deepEqual(await runMigrationsOn(db, [USERS, PAIRS]), [USERS.name, PAIRS.name]);
  const ORPHAN = { name: "20261108120000_orphan.sql", sql: "insert into refs values (1, 3, 'orphan');" };
  await assert.rejects(runMigrationsOn(db, [USERS, PAIRS, ORPHAN]), /rolled back: 1 rows break a foreign key after this migration$/);
  const GONE = { name: "20261108120000_gone.sql", sql: "create table stale (id integer references nowhere (id)); insert into stale values (1);" };
  await assert.rejects(runMigrationsOn(db, [USERS, PAIRS, GONE]), /rolled back: 1 rows break a foreign key after this migration$/);
});

test("migrate(db) takes a migration another start of the app ran after it read the history as run", async () => {
  const db = new DatabaseSync(":memory:");
  await runMigrationsOn(db, [USERS]);
  // As a second process sees it: the history read before the first committed BIO, the row there by its turn.
  let reads = 0;
  const racing: MigrationConnection = {
    exec: (sql: string) => db.exec(sql),
    prepare(sql: string) {
      const statement = db.prepare(sql);
      if (!sql.startsWith("select name, sql from") || reads++) return statement;
      return { all: () => { const rows = statement.all(); runMigrations(storage(db), [USERS, BIO]); return rows; }, run: statement.run.bind(statement) };
    },
  };
  assert.deepEqual(await runMigrationsOn(racing, [USERS, BIO]), []);
  assert.deepEqual(await names(db), [USERS.name, BIO.name]);
});

test("migrate(db) refuses a history the project's migrations don't continue, running nothing", async () => {
  const db = await driver("@tursodatabase/database").connect(":memory:");
  await runMigrationsOn(db, [USERS, BIO]);
  await assert.rejects(runMigrationsOn(db, [USERS]), /^Error: reylo: this database ran the migration 20261106120000_bio\.sql, which the project doesn't have: put it back in the project's migrations/);
  await assert.rejects(runMigrationsOn(db, [USERS, { ...BIO, sql: "alter table users add column bio text not null default '';" }]), /^Error: reylo: the migration 20261106120000_bio\.sql isn't the text this database ran/);
  assert.deepEqual(await names(db), [USERS.name, BIO.name]);
});

test("migrate(db) on Turso's engine runs its own SQL, a full-text index included", async () => {
  const db = await driver("@tursodatabase/database").connect(":memory:", { experimental: ["index_method"] });
  const FTS = { name: "20261107120000_fts.sql", sql: "create index users_name_fts on users using fts (name);" };
  assert.deepEqual(await runMigrationsOn(db, [USERS, FTS]), [USERS.name, FTS.name]);
  await db.exec("insert into users (name) values ('kiwi venture'), ('other')");
  const found = (await (await db.prepare("select name from users where fts_match(name, 'kiwi')")).all()) as { name: string }[];
  assert.deepEqual(found.map((r) => r.name), ["kiwi venture"]);
});

test("migrate refuses a client it can't run migrations on, rather than deploying to the project's database", async () => {
  // As @libsql/client's, which has execute() and no prepare().
  const client = { execute: async () => ({ rows: [] }), close() {} };
  await assert.rejects((migrate as (arg: unknown) => Promise<string[]>)(client), /^TypeError: reylo: migrate takes a Durable Object's ctx\.storage, an app's own SQLite connection/);
});

test("migrate(db) refuses a pending migration with its own commit before running any, and leaves one it ran alone", async () => {
  const TOPUP = { name: "20261106120000_topup.sql", sql: "insert into users (name) values ('ada');\ncommit;\ninsert into nowhere values (1);\n" };
  const db = new DatabaseSync(":memory:");
  await assert.rejects(runMigrationsOn(db, [USERS, TOPUP]), /^Error: reylo: the migration 20261106120000_topup\.sql can't run atomically: it has its own commit, and Reylo runs each migration in a transaction of its own, so take it out$/);
  assert.deepEqual(await names(db), []);
  // A comment is no statement, whatever it says, as SQLite reads it.
  const NOTED = { name: "20261106120000_noted.sql", sql: "--Rollback: drop table bio;\nalter table users add column bio text; -- End of file\n" };
  assert.deepEqual(await runMigrationsOn(db, [USERS, NOTED]), [USERS.name, NOTED.name]);
});

test("the key check Turso's engine runs counts the rows SQLite's foreign_key_check counts, on the same schemas", async () => {
  const schemas = [
    "create table p (id text primary key); create table c (p int references p (id)); insert into p values ('01'); insert into c values (1);",
    "create table p (id primary key); create table c (p int references p (id)); insert into p values ('1'); insert into c values (1);",
    "create table p (id int primary key); create table c (p text references p (id)); insert into p values (1); insert into c values ('1'), ('2');",
    "create table p (name text collate nocase unique); create table c (p text references p (name)); insert into p values ('Ada'); insert into c values ('ADA'), ('Grace');",
    "create table p (a int, b int, primary key (a, b)) without rowid; create table c (x int, y int, foreign key (x, y) references p); insert into p values (1, 2); insert into c values (1, 2), (1, 3), (null, 3);",
    "create table t (id integer primary key, up int references t (id)); insert into t values (1, null), (2, 1), (3, 9);",
    'create table "odd ""name" (id integer primary key); create table c (p int references "odd ""name" (id), q int references gone (id)); insert into c values (1, 1);',
  ];
  for (const schema of schemas) {
    const db = new DatabaseSync(":memory:");
    db.exec(`pragma foreign_keys = off; ${schema}`);
    const rows = async (sql: string, values: unknown[] = []) => db.prepare(sql).all(...(values as [])) as unknown[];
    assert.equal(await brokenKeys(rows), db.prepare("pragma foreign_key_check").all().length, schema);
  }
  // A key SQLite can't hold, which fails its check: to a column that's no key, a part of one, a view, or rowid.
  const mismatched = [
    "create table p (a int, b int); create table c (x references p (a)); insert into c values (1);",
    "create table p (a int, b int, primary key (a, b)); create table c (x references p); insert into c values (1);",
    "create table p (a int unique, b int); create unique index p_b on p (b) where b > 0; create table c (x references p (b)); insert into c values (1);",
    "create table t (id integer primary key); create view v as select id from t; create table c (x references v (id)); insert into c values (1);",
    "create table t (id int); create table c (x references t (rowid)); insert into c values (1);",
    "create table p (a int, b int, unique (a, b)); create table c (x int, y int, foreign key (x, y) references p (a, a)); insert into c values (1, 1);",
  ];
  for (const schema of mismatched) {
    const db = new DatabaseSync(":memory:");
    db.exec(`pragma foreign_keys = off; ${schema}`);
    const rows = async (sql: string, values: unknown[] = []) => db.prepare(sql).all(...(values as [])) as unknown[];
    assert.throws(() => db.prepare("pragma foreign_key_check").all(), /foreign key mismatch/, schema);
    await assert.rejects(brokenKeys(rows), /^Error: foreign key mismatch - "c" referencing /, schema);
  }
});

test("migrate(db) refuses, on Turso's engine, a migration that may turn foreign keys on inside its transaction", async () => {
  const PARENT = { name: "20261107120000_posts.sql", sql: "create table posts (id integer primary key, user_id integer references users (id) on delete cascade); insert into users (name) values ('ada'); insert into posts (user_id) values (1);" };
  const REBUILD = { name: "20261108120000_rebuild.sql", sql: "pragma foreign_keys = on; create table users_new (id integer primary key, name text not null); insert into users_new select * from users; drop table users; alter table users_new rename to users;" };
  const turso = await driver("@tursodatabase/database").connect(":memory:");
  assert.deepEqual(await runMigrationsOn(turso, [USERS, PARENT]), [USERS.name, PARENT.name]);
  await assert.rejects(runMigrationsOn(turso, [USERS, PARENT, REBUILD]), /^Error: reylo: the migration 20261108120000_rebuild\.sql sets foreign_keys to something other than off, which Turso's engine takes inside the migration's transaction/);
  // On, then off again before the check: refused before it runs, so nothing cascades.
  const ON_OFF = { ...REBUILD, sql: REBUILD.sql.replace("pragma foreign_keys = on; ", "pragma foreign_keys = 0x1; ") + " pragma foreign_keys = off;" };
  await assert.rejects(runMigrationsOn(turso, [USERS, PARENT, ON_OFF]), /^Error: reylo: the migration 20261108120000_rebuild\.sql sets foreign_keys to something other than off/);
  // A way of turning them on the preflight doesn't read, as this connection does with the migration's
  // own text, is caught once the migration ran, inside its transaction, whose rollback undoes the cascade.
  const SILENT = { ...REBUILD, sql: REBUILD.sql.replace("pragma foreign_keys = on; ", "") };
  const sneaky: MigrationConnection = { exec: (sql: string) => turso.exec(sql === SILENT.sql ? `pragma foreign_keys = on; ${sql}` : sql), prepare: (sql: string) => turso.prepare(sql) };
  await assert.rejects(runMigrationsOn(sneaky, [USERS, PARENT, SILENT]), /^Error: reylo: the migration 20261108120000_rebuild\.sql failed on this database, and rolled back: it turned foreign keys on/);
  const posts = (await (await turso.prepare("select count(*) as n from posts")).all()) as { n: number }[];
  assert.equal(Number(posts[0]!.n), 1);
  // SQLite takes the pragma only outside a transaction, so there it runs, and cascades to nothing.
  const db = new DatabaseSync(":memory:");
  db.exec("pragma foreign_keys = on");
  assert.deepEqual(await runMigrationsOn(db, [USERS, PARENT, REBUILD]), [USERS.name, PARENT.name, REBUILD.name]);
  assert.equal(Number((db.prepare("select count(*) as n from posts").get() as { n: number }).n), 1);
});

test("migrate(db) refuses an init.sql with its own commit, which runs in a transaction like any migration", async () => {
  const INIT = { name: "init.sql", sql: "create table users (id integer primary key);\ncommit;\ncreate table nowhere (x);\n" };
  await assert.rejects(runMigrationsOn(new DatabaseSync(":memory:"), [INIT]), /^Error: reylo: the migration init\.sql can't run atomically: it has its own commit/);
});

test("migrate(db) holds the whole history again under the transaction's lock, and refuses one another start changed", async () => {
  const db = new DatabaseSync(":memory:");
  await runMigrationsOn(db, [USERS]);
  const AGE = { name: "20261106130000_age.sql", sql: "alter table users add column age int;" };
  // As a second process with another build sees it: after its history read, the first commits AGE, which this build lacks.
  let reads = 0;
  const racing: MigrationConnection = {
    exec: (sql: string) => db.exec(sql),
    prepare(sql: string) {
      const statement = db.prepare(sql);
      if (!sql.startsWith("select name, sql from") || reads++) return statement;
      return { all: () => { const rows = statement.all(); runMigrations(storage(db), [USERS, AGE]); return rows; }, run: statement.run.bind(statement) };
    },
  };
  await assert.rejects(runMigrationsOn(racing, [USERS, BIO]), /^Error: reylo: this database ran the migration 20261106130000_age\.sql, which the project doesn't have/);
  assert.deepEqual(await names(db), [USERS.name, AGE.name]);
});
