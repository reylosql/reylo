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
import type { Inputs } from "../src/execute.ts";
import { DatabaseSync } from "node:sqlite";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { handWritten, mysqlDatabase, mysqlLive, pgLive, postgresDatabase } from "./helpers.ts";
import { array, pg } from "./descriptors.ts";

// Reylo's queries inside Effect SQL: on the SqlClient the Effect's context provides, on the open
// transaction's connection inside withTransaction, every value decoded by Reylo as `.run()`
// decodes it, a database error as Effect's SqlError and a value Reylo refuses as a defect.

const sqlite = <Params, Row, C extends "many" | "one" | "maybeOne" | "none" = "many">(sql: string, columns?: Columns, cardinality?: C, value?: boolean) =>
  defineQuery<Params, Row, C, "sqlite">({ dialect: "sqlite", ...handWritten(sql, "sqlite"), ...(columns && { columns }), ...(cardinality && { cardinality }), ...(value && { value }) });

const postgres = <Params, Row, C extends "many" | "one" | "maybeOne" | "none" = "many">(sql: string, columns?: Columns, cardinality?: C, inputs?: Inputs) =>
  defineQuery<Params, Row, C, "postgres">({ dialect: "postgres", ...handWritten(sql, "postgres"), ...(columns && { columns }), ...(cardinality && { cardinality }), ...(inputs && { inputs }) });

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
  assert.match(String((rejected as Error).message), /an Effect SQL client: \.run\(\) takes a driver's own client\. Inside an Effect, run the query with effect\(query, params\) from reylo\/effect, on Effect 4\.1\.6 or later\./);
});


// The `typed` fixture of lab/driver-matrix (seed/*.sql, with the columns its TYPED_COLUMNS give
// each dialect): one value of each SQL type, read through Effect and through `.run()` on the driver
// underneath, which must agree value for value, compared with their types (mysql2's and postgres.js's
// bytes are a Buffer, node:sqlite's a Uint8Array, on either path); then the row goes back in
// as the parameters of a statement comparing each column with one, which must hold for every column.

test("Effect's node:sqlite client gives the driver matrix's typed row as .run() gives it on node:sqlite, and takes it back", async () => {
  const file = join(mkdtempSync(join(tmpdir(), "reylo-effect-")), "app.db");
  const db = new DatabaseSync(file);
  db.exec("create table typed (id integer primary key, n integer, r real, s text, b blob, doc text, flag integer, x, big integer)");
  db.exec("insert into typed values (1, 5, 1.5, 'hi', x'0102ff', '{\"a\":[1,2]}', 1, 7, 9007199254740993)");
  const get = sqlite<{ id: number }, Record<string, unknown>, "one">("select id, n, r, s, b, doc, flag, x, big from typed where id = :id", [["id", "int"], ["n", "int"], ["r", "float"], ["s", "text"], ["b", "bytes"], ["doc", "json"], ["flag", "bool"], ["x", "unknown"], ["big", "int8"]], "one");
  const back = sqlite<Record<string, unknown>, unknown, "one">("select n = :n and r = :r and s = :s and b = :b and flag = :flag and x = :x and big = :big as same from typed where id = 1", [["same", "bool"]], "one");
  try {
    const direct = await get.run(db, { id: 1 });
    const program = Effect.gen(function* () {
      const row = yield* effect(get, { id: 1 });
      const { id: _, doc: __, ...rest } = row;
      return [row, yield* effect(back, rest)] as const;
    });
    const [row, same] = await Effect.runPromise(program.pipe(Effect.provide(SqliteClient.layer({ filename: file }))));
    assert.deepStrictEqual(row, direct);
    assert.deepStrictEqual({ ...row }, { id: 1, n: 5, r: 1.5, s: "hi", b: Uint8Array.of(1, 2, 255), doc: { a: [1, 2] }, flag: true, x: 7, big: 9007199254740993n });
    assert.deepStrictEqual({ ...(same as object) }, { same: true });
  } finally {
    db.close();
  }
});

test("Effect's mysql2 client gives the driver matrix's typed row as .run() gives it on mysql2, takes it back, and runs on the transaction's connection", { skip: mysqlLive }, async () => {
  const database = await mysqlDatabase(
    "reylo_effect",
    `create table typed (id int primary key, big bigint, ubig bigint unsigned, amount decimal(30,18), day date, at datetime(3), doc json, bit1 bit(1), raw varbinary(4), tags set('x','y','z'), kind enum('a','b'), flag tinyint(1), yr year, f8 double, tm time, s text);
     insert into typed values (1, -9007199254740993, 18446744073709551615, 1.100000000000000001, '2026-10-20', '2026-10-20 10:00:00.123', '{"a": [1, 2]}', b'1', x'0102ff', 'x,z', 'b', 1, 2026, 2.5, '10:00:00', 'hi');
     create table uniq (k int primary key);`,
  );
  try {
    const columns: Columns = [["id", "int"], ["big", "int8"], ["ubig", "int8"], ["amount", "decimal"], ["day", "date"], ["at", "timestamp"], ["doc", "json"], ["bit1", "bool"], ["raw", "bytes"], ["tags", "set"], ["kind", "enum"], ["flag", "bool"], ["yr", "int"], ["f8", "float"], ["tm", "text"], ["s", "text"]];
    const get = mysql<{}, Record<string, unknown>, "one">(`select ${columns.map(([name]) => name).join(", ")} from typed where id = 1`, columns, "one");
    const back = mysql<Record<string, unknown>, unknown, "one">("select big = :big and ubig = :ubig and amount = :amount and day = :day and at = :at and raw = :raw and kind = :kind and yr = :yr and f8 = :f8 and tm = :tm and s = :s as same from typed where id = 1", [["same", "bool"]], "one");
    const insert = mysql<{ k: number }, never, "none">("insert into uniq (k) values (:k)", undefined, "none");
    const count = mysql<{}, bigint, "one">("select count(*) from uniq", [["count(*)", "int8"]], "one", true);
    const direct = await get.run(database.db);
    const program = Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const row = yield* effect(get);
      const same = yield* effect(back, { big: row.big, ubig: row.ubig, amount: row.amount, day: row.day, at: row.at, raw: row.raw, kind: row.kind, yr: row.yr, f8: row.f8, tm: row.tm, s: row.s });
      yield* effect(insert, { k: 1 });
      const inside = yield* sql
        .withTransaction(
          Effect.gen(function* () {
            yield* effect(insert, { k: 2 });
            return yield* Effect.fail(yield* effect(count));
          }),
        )
        .pipe(Effect.flip);
      const duplicate = yield* Effect.flip(effect(insert, { k: 1 }));
      return { row, same, inside, after: yield* effect(count), duplicate };
    });
    const { row, same, inside, after, duplicate } = await Effect.runPromise(program.pipe(Effect.provide(MysqlClient.layer({ url: Redacted.make(database.url) }))));
    assert.deepStrictEqual(row, direct);
    assert.deepStrictEqual({ ...row }, { id: 1, big: -9007199254740993n, ubig: 18446744073709551615n, amount: "1.100000000000000001", day: "2026-10-20", at: "2026-10-20T10:00:00.123", doc: { a: [1, 2] }, bit1: true, raw: Buffer.from([1, 2, 255]), tags: new Set(["x", "z"]), kind: "b", flag: true, yr: 2026, f8: 2.5, tm: "10:00:00", s: "hi" });
    assert.deepStrictEqual({ ...(same as object) }, { same: true });
    assert.deepEqual([inside, after], [2n, 1n]);
    assert.ok(SqlError.isSqlError(duplicate));
    assert.equal(duplicate.reason._tag, "UniqueViolation");
    assert.equal((duplicate.reason as SqlError.UniqueViolation).constraint, "uniq.PRIMARY");
  } finally {
    await database.drop();
  }
});

test("Effect's pg client gives the driver matrix's typed row as .run() gives it on postgres.js, through queryTextValues(), and takes it back", { skip: pgLive }, async () => {
  const database = await postgresDatabase(
    "reylo_effect",
    `create type mood as enum ('ok', 'sad');
     create type pair as (a int, b text);
     create table typed (id int primary key, big int8, amount numeric, day date, at timestamp(3), stamp timestamptz, doc json, docb jsonb, raw bytea, flag bool, f8 float8, tags text[], ints int4[], moods mood[], span interval, uid uuid, pr pair, pt point, bxs box[], seats int4range, many int8multirange);
     insert into typed values (1, 9007199254740993, 12345678901234567890.123456789, '2026-10-20', '2026-10-20 10:00:00.123', '2026-10-20 10:00:00.123+02', '{"a": [1, 2]}', '{"b": true}', '\\x0102ff', true, 2.5, '{a,NULL,"NULL","b,c"}', '{1,2,3}', '{ok,sad}', '1 day 2 hours', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', row(1, 'x,y'), '(1.5,-2)', '{(3,4),(1,2);(1,1),(0,0)}', '[1,5)', '{[1,3),[5,7)}'), (2, null, 'NaN', null, null, 'infinity', null, null, null, null, null, null, null, null, null, null, null, null, null, null, null);
     create table uniq (k int primary key);`,
  );
  try {
    const columns: Columns = [["id", "int"], ["big", "int8"], ["amount", "decimal"], ["day", "date"], ["at", "timestamp"], ["stamp", "timestamptz"], ["doc", "json"], ["docb", "json"], ["raw", "bytes"], ["flag", "bool"], ["f8", "float"], ["tags", "text[]"], ["ints", "int[]"], ["moods", "text[]"], ["span", "interval"], ["uid", "text"], ["pr", '(["a","int"],["b","text"])'], ["pt", "point"], ["bxs", "box[]", ";"], ["seats", "range:int"], ["many", "multirange:int8"]];
    const get = postgres<{ id: number }, Record<string, unknown>, "one">(`select ${columns.map(([name]) => name).join(", ")} from typed where id = :id`, columns, "one");
    const inputs: Inputs = { big: { type: pg("int8") }, amount: { type: pg("numeric") }, day: { type: pg("date") }, at: { type: pg("timestamp") }, stamp: { type: pg("timestamptz") }, docb: { type: { kind: "json" } }, raw: { type: pg("bytea") }, flag: { type: pg("bool") }, f8: { type: pg("float8") }, ints: { type: array(pg("int4")) }, span: { type: pg("interval") }, uid: { type: pg("uuid") } };
    const back = postgres<Record<string, unknown>, unknown, "one">("select big = :big and amount = :amount and day = :day and at = :at and stamp = :stamp and docb = :docb and raw = :raw and flag = :flag and f8 = :f8 and ints = :ints and span = :span and uid = :uid as same from typed where id = 1", [["same", "bool"]], "one", inputs);
    const insert = postgres<{ k: number }, never, "none">("insert into uniq (k) values (:k)", undefined, "none");
    const count = postgres<{}, unknown, "one">("select count(*)::int as n from uniq", [["n", "int"]], "one");
    const direct = [await get.run(database.sql, { id: 1 }), await get.run(database.sql, { id: 2 })];
    const program = Effect.gen(function* () {
      const sql = yield* SqlClient.SqlClient;
      const rows = [yield* effect(get, { id: 1 }), yield* effect(get, { id: 2 })];
      const r = rows[0]!;
      const same = yield* effect(back, { big: r.big, amount: r.amount, day: r.day, at: r.at, stamp: r.stamp, docb: r.docb, raw: r.raw, flag: r.flag, f8: r.f8, ints: r.ints, span: r.span, uid: r.uid });
      yield* effect(insert, { k: 1 });
      const inside = yield* sql
        .withTransaction(
          Effect.gen(function* () {
            yield* effect(insert, { k: 2 });
            return yield* Effect.fail(yield* effect(count));
          }),
        )
        .pipe(Effect.flip);
      const duplicate = yield* Effect.flip(effect(insert, { k: 1 }));
      return { rows, same, inside, after: yield* effect(count), duplicate };
    });
    const { rows, same, inside, after, duplicate } = await Effect.runPromise(program.pipe(Effect.provide(PgClient.layer({ url: Redacted.make(database.url) }))));
    assert.deepStrictEqual(rows, direct);
    assert.deepStrictEqual({ ...rows[0] }, { id: 1, big: 9007199254740993n, amount: "12345678901234567890.123456789", day: "2026-10-20", at: "2026-10-20T10:00:00.123000", stamp: "2026-10-20T08:00:00.123000Z", doc: { a: [1, 2] }, docb: { b: true }, raw: Buffer.from([1, 2, 255]), flag: true, f8: 2.5, tags: ["a", null, "NULL", "b,c"], ints: [1, 2, 3], moods: ["ok", "sad"], span: "P1DT2H", uid: "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11", pr: { a: 1, b: "x,y" }, pt: { x: 1.5, y: -2 }, bxs: [{ high: { x: 3, y: 4 }, low: { x: 1, y: 2 } }, { high: { x: 1, y: 1 }, low: { x: 0, y: 0 } }], seats: { lower: 1, upper: 5, bounds: "[)" }, many: [{ lower: 1n, upper: 3n, bounds: "[)" }, { lower: 5n, upper: 7n, bounds: "[)" }] });
    assert.equal((rows[1] as Record<string, unknown>).stamp, "infinity");
    assert.equal((rows[1] as Record<string, unknown>).amount, "NaN");
    assert.deepStrictEqual({ ...(same as object) }, { same: true });
    assert.deepEqual([inside, after], [{ n: 2 }, { n: 1 }]);
    assert.ok(SqlError.isSqlError(duplicate));
    assert.equal(duplicate.reason._tag, "UniqueViolation");
  } finally {
    await database.drop();
  }
});
