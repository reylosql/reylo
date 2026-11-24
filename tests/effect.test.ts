import { test } from "node:test";
import assert from "node:assert/strict";
import { Cause, Effect, Exit, Redacted } from "effect";
import { SqlClient, SqlError } from "effect/sql";
import { SqliteClient } from "@effect/sql-sqlite-node";
import { MysqlClient } from "@effect/sql-mysql2";
import { LibsqlClient } from "@effect/sql-libsql";
import { PgClient } from "@effect/sql-pg";
import { defineQuery } from "../src/index.ts";
import { effect } from "../src/effect.ts";
import type { Columns } from "../src/values.ts";
import { handWritten, MYSQL, PG } from "./helpers.ts";

// Reylo's queries inside Effect SQL: on the SqlClient the Effect's context provides, on the open
// transaction's connection inside withTransaction, every value decoded by Reylo as `.run()`
// decodes it, a database error as Effect's SqlError and a value Reylo refuses as a defect.

const sqlite = <Params, Row, C extends "many" | "one" | "maybeOne" | "none" = "many">(sql: string, columns?: Columns, cardinality?: C, value?: boolean) =>
  defineQuery<Params, Row, C, "sqlite">({ dialect: "sqlite", ...handWritten(sql, "sqlite"), ...(columns && { columns }), ...(cardinality && { cardinality }), ...(value && { value }) });

const postgres = <Params, Row, C extends "many" | "one" | "maybeOne" | "none" = "many">(sql: string, columns?: Columns, cardinality?: C) =>
  defineQuery<Params, Row, C, "postgres">({ dialect: "postgres", ...handWritten(sql, "postgres"), ...(columns && { columns }), ...(cardinality && { cardinality }) });

const mysql = <Params, Row, C extends "many" | "one" | "maybeOne" | "none" = "many">(sql: string, columns?: Columns, cardinality?: C, value?: boolean) =>
  defineQuery<Params, Row, C, "mysql">({ dialect: "mysql", ...handWritten(sql, "mysql"), ...(columns && { columns }), ...(cardinality && { cardinality }), ...(value && { value }) });

/** The defect an Effect died with, or a failure if it ended otherwise. */
function defect(exit: Exit.Exit<unknown, unknown>): unknown {
  if (Exit.isSuccess(exit)) assert.fail("the Effect succeeded");
  const defects = exit.cause.reasons.filter(Cause.isDieReason);
  assert.equal(defects.length, 1, String(exit.cause));
  return defects[0]!.defect;
}

const sqliteLayer = () => SqliteClient.layer({ filename: ":memory:" });

test("a SQLite query on Effect's node:sqlite client decodes every value as .run() does, BigInts past 2^53 exact", async () => {
  const get = sqlite<{ id: number }, unknown, "maybeOne">("select id, big, b, ok, j, id * 10 as id from t where id = :id", [["id", "int"], ["big", "int8"], ["b", "bytes"], ["ok", "bool"], ["j", "json"], ["id", "int"]], "maybeOne");
  const program = Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql.unsafe("create table t (id integer primary key, big integer, b blob, ok integer, j text)");
    yield* sql.unsafe("insert into t values (1, 9007199254740993, x'0102', 1, '{\"a\":[1,2]}')");
    return [yield* effect(get, { id: 1 }), yield* effect(get, { id: 2 })];
  });
  const [row, missing] = await Effect.runPromise(program.pipe(Effect.provide(sqliteLayer())));
  // Two columns named id: the row keeps the last, read by position.
  assert.deepEqual(row, { id: 10, big: 9007199254740993n, b: new Uint8Array([1, 2]), ok: true, j: { a: [1, 2] } });
  assert.equal(missing, null);
});

test("a SQLite query on Effect's libSQL client, which reads no BigInt by default, gives a BLOB as bytes and a BIGINT as a BigInt", async () => {
  const get = sqlite<{}, unknown>("select id, big, b from t order by id", [["id", "int"], ["big", "int8"], ["b", "bytes"]]);
  const program = Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql.unsafe("create table t (id integer primary key, big integer, b blob)");
    yield* sql.unsafe("insert into t values (1, 5, x'0102'), (2, 9007199254740993, null)");
    return yield* effect(get);
  });
  // Under intMode "bigint" every INTEGER is a BigInt, which Reylo gives an INTEGER column as a number.
  const rows = await Effect.runPromise(program.pipe(Effect.provide(LibsqlClient.layer({ url: "file::memory:", intMode: "bigint" }))));
  assert.deepEqual(rows, [
    { id: 1, big: 5n, b: new Uint8Array([1, 2]) },
    { id: 2, big: 9007199254740993n, b: null },
  ]);
});

test("a SQLite query runs on the transaction's connection inside withTransaction", async () => {
  const insert = sqlite<{ id: number }, never, "none">("insert into t (id) values (:id)", undefined, "none");
  const count = sqlite<{}, number, "one">("select count(*) from t", [["count(*)", "int"]], "one", true);
  const program = Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql.unsafe("create table t (id integer primary key)");
    const inside = yield* sql
      .withTransaction(
        Effect.gen(function* () {
          yield* effect(insert, { id: 1 });
          return yield* Effect.fail(yield* effect(count));
        }),
      )
      .pipe(Effect.flip);
    return [inside, yield* effect(count)];
  });
  assert.deepEqual(await Effect.runPromise(program.pipe(Effect.provide(sqliteLayer()))), [1, 0]);
});

test("a SQLite database error fails as Effect's SqlError, and a value Reylo refuses is a defect", async () => {
  const insert = sqlite<{ id: number; ok: number }, never, "none">("insert into t (id, ok) values (:id, :ok)", undefined, "none");
  const get = sqlite<{ id: number }, unknown, "one">("select ok from t where id = :id", [["ok", "bool"]], "one");
  const program = Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql.unsafe("create table t (id integer primary key, ok integer)");
    yield* effect(insert, { id: 1, ok: 7 });
    const duplicate = yield* Effect.flip(effect(insert, { id: 1, ok: 1 }));
    return [duplicate, yield* Effect.exit(effect(get, { id: 1 })), yield* Effect.exit(effect(get, { id: 2 }))] as const;
  });
  const [duplicate, refused, none] = await Effect.runPromise(program.pipe(Effect.provide(sqliteLayer())));
  assert.ok(SqlError.isSqlError(duplicate));
  assert.equal(duplicate.reason._tag, "ConstraintError");
  assert.match(String((defect(refused) as Error).message), /Column "ok" is a boolean, stored as 0 or 1, and holds 7/);
  assert.match(String((defect(none) as Error).message), /Expected exactly one row, got 0/);
});

test("a SQLite query whose result has another width than it declares is refused, and one without columns reads the driver's names", async () => {
  const declared = sqlite<{}, unknown>("select * from t", [["id", "int"]]);
  const bare = defineQuery<{}, unknown, "many", "sqlite">({ dialect: "sqlite", sql: "select id, id + 1 as next from t", params: [] });
  const program = Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    yield* sql.unsafe("create table t (id integer primary key, extra text)");
    yield* sql.unsafe("insert into t values (1, 'x')");
    return [yield* Effect.exit(effect(declared)), yield* effect(bare)] as const;
  });
  const [wider, rows] = await Effect.runPromise(program.pipe(Effect.provide(sqliteLayer())));
  assert.match(String((defect(wider) as Error).message), /returned 2 columns where the query declares 1/);
  assert.deepEqual(
    (rows as Record<string, unknown>[]).map((r) => ({ ...r })),
    [{ id: 1, next: 2 }],
  );
});

test("a query for another database is a defect, naming the clients its database runs on", async () => {
  const q = mysql<{}, unknown>("select 1 as one", [["one", "int"]]);
  const exit = await Effect.runPromise(Effect.exit(effect(q)).pipe(Effect.provide(sqliteLayer())));
  assert.match(String((defect(exit) as Error).message), /cannot run a mysql statement on Effect's sqlite-node client: it runs mysql statements on Effect's mysql2/);
});

test(".run() refuses an Effect SQL client, whose unsafe() hands back an Effect rather than rows", async () => {
  const q = sqlite<{}, unknown>("select 1 as one", [["one", "int"]]);
  const rejected = await Effect.runPromise(
    Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      return yield* Effect.promise(() => q.run(sql as never).then(() => undefined, (error: unknown) => error));
    }).pipe(Effect.provide(sqliteLayer())),
  );
  assert.match(String((rejected as Error).message), /with \.run\(\), which takes a driver's own client: inside an Effect, run the query with effect\(query, params\) from reylo\/effect/);
});

const mysqlSkip = !MYSQL ? "REYLO_TEST_MYSQL names no server" : false;

/** A database of the test's own on the test MySQL, made and dropped through Effect's client. */
function mysqlLayer(name: string) {
  const at = (database: string) => {
    const url = new URL(MYSQL!);
    url.pathname = `/${database}`;
    return MysqlClient.layer({ url: Redacted.make(url.href) });
  };
  const admin = (statement: string) =>
    Effect.runPromise(
      Effect.gen(function* () {
        const sql = yield* SqlClient.SqlClient;
        yield* sql.unsafe(statement);
      }).pipe(Effect.provide(at(""))),
    );
  return { at: () => at(name), make: () => admin(`drop database if exists ${name}`).then(() => admin(`create database ${name}`)), drop: () => admin(`drop database if exists ${name}`) };
}

test("a MySQL query on Effect's mysql2 client runs on mysql2's connection with Reylo's options", { skip: mysqlSkip }, async () => {
  const database = mysqlLayer(`reylo_effect_${process.pid}`);
  await database.make();
  try {
    const get = mysql<{ id: number }, unknown, "one">("select id, big, d, ts, `dec`, j, ok from t where id = :id", [["id", "int"], ["big", "int8"], ["d", "date"], ["ts", "timestamp"], ["dec", "decimal"], ["j", "json"], ["ok", "bool"]], "one");
    const insert = mysql<{ id: number }, never, "none">("insert into t (id) values (:id)", undefined, "none");
    const count = mysql<{}, bigint, "one">("select count(*) from t", [["count(*)", "int8"]], "one", true);
    const program = Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* sql.unsafe("create table t (id int primary key, big bigint, d date, ts datetime(6), `dec` decimal(30,10), j json, ok tinyint(1))");
      yield* sql.unsafe("insert into t values (1, 9007199254740993, '2026-11-24', '2026-11-24 10:11:12.345678', 12345678901234567890.0123456789, '{\"a\":1.10}', 1)");
      const row = yield* effect(get, { id: 1 });
      const inside = yield* sql
        .withTransaction(
          Effect.gen(function* () {
            yield* effect(insert, { id: 2 });
            return yield* Effect.fail(yield* effect(count));
          }),
        )
        .pipe(Effect.flip);
      const duplicate = yield* Effect.flip(effect(insert, { id: 1 }));
      return { row, inside, after: yield* effect(count), duplicate };
    });
    const { row, inside, after, duplicate } = await Effect.runPromise(program.pipe(Effect.provide(database.at())));
    assert.deepEqual(row, { id: 1, big: 9007199254740993n, d: "2026-11-24", ts: "2026-11-24T10:11:12.345678", dec: "12345678901234567890.0123456789", j: { a: 1.1 }, ok: true });
    assert.deepEqual([inside, after], [2n, 1n]);
    assert.ok(SqlError.isSqlError(duplicate));
    assert.equal(duplicate.reason._tag, "UniqueViolation");
    assert.equal((duplicate.reason as SqlError.UniqueViolation).constraint, "t.PRIMARY");
  } finally {
    await database.drop();
  }
});

const pgSkip = !PG ? "REYLO_TEST_PG names no server" : false;

test("a Postgres query on Effect's pg client reads each value from the server's text, through queryText()", { skip: pgSkip }, async () => {
  const name = `reylo_effect_${process.pid}`;
  const at = (database: string) => {
    const url = new URL(PG!);
    url.pathname = `/${database}`;
    return PgClient.layer({ url: Redacted.make(url.href) });
  };
  const admin = (statement: string) =>
    Effect.runPromise(
      Effect.gen(function* () {
        const sql = yield* SqlClient.SqlClient;
        yield* sql.unsafe(statement);
      }).pipe(Effect.provide(at(new URL(PG!).pathname.slice(1) || "postgres"))),
    );
  await admin(`drop database if exists ${name}`);
  await admin(`create database ${name}`);
  try {
    const get = postgres<{ id: number }, unknown, "one">("select id, at, big, n, iv, j, moods from t where id = :id", undefined, "one");
    const insert = postgres<{ id: number }, never, "none">("insert into t (id) values (:id)", undefined, "none");
    const count = postgres<{}, unknown, "one">("select count(*)::int as n from t", [["n", "int"]], "one");
    const program = Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      yield* sql.unsafe("create type mood as enum ('ok', 'meh')");
      yield* sql.unsafe("create table t (id int primary key, at timestamptz, big int8, n numeric, iv interval, j jsonb, moods mood[])");
      yield* sql.unsafe("insert into t values (1, '2026-11-24 10:11:12.345678+00', 9007199254740993, 'NaN', '1 year 2 mons', '{\"a\":2.50}', '{ok,meh}'), (2, 'infinity', null, null, null, null, null)");
      const rows = [yield* effect(get, { id: 1 }), yield* effect(get, { id: 2 })];
      const inside = yield* sql
        .withTransaction(
          Effect.gen(function* () {
            yield* effect(insert, { id: 3 });
            return yield* Effect.fail(yield* effect(count));
          }),
        )
        .pipe(Effect.flip);
      const duplicate = yield* Effect.flip(effect(insert, { id: 1 }));
      return { rows, inside, after: yield* effect(count), duplicate };
    });
    const exit = await Effect.runPromise(Effect.exit(program).pipe(Effect.provide(at(name))));
    if (!("queryText" in (await Effect.runPromise(SqlClient.SqlClient.pipe(Effect.provide(at(name))))))) {
      // A release of @effect/sql-pg before queryText(): its own codecs decode every value, which Reylo cannot read back.
      assert.match(String((defect(exit) as Error).message), /this version of @effect\/sql-pg has none\. Update @effect\/sql-pg/);
      return;
    }
    assert.ok(Exit.isSuccess(exit), String(Exit.isFailure(exit) && exit.cause));
    const { rows, inside, after, duplicate } = exit.value;
    assert.deepEqual(rows, [
      { id: 1, at: "2026-11-24T10:11:12.345678Z", big: 9007199254740993n, n: "NaN", iv: "P1Y2M", j: { a: 2.5 }, moods: ["ok", "meh"] },
      { id: 2, at: "infinity", big: null, n: null, iv: null, j: null, moods: null },
    ]);
    assert.deepEqual([inside, after], [{ n: 3 }, { n: 2 }]);
    assert.ok(SqlError.isSqlError(duplicate));
    assert.equal(duplicate.reason._tag, "UniqueViolation");
  } finally {
    await admin(`drop database if exists ${name}`);
  }
});
