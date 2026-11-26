// Runtime execution over a caller-supplied client, typed structurally so that
// reylo has no dependency (not even a type-only one) on any driver package.
// Each type is the smallest slice of the real driver that execution needs, and
// the query's dialect selects which clients are accepted at compile time. A
// transaction, savepoint, reserved or checked-out connection has the same shape
// as the client it came from, so it is accepted wherever that client is.
//
// Each database's drivers, errors and raw values are its own module's (`Runtime` below), which
// this module never imports: a query reaches them through its module, so that a bundle holds only
// the databases its queries are written for.

import type { QueryError } from "./query-error.ts";
import { columnKind, describe, given, ParameterError, vectorText, zoned, type Columns, type JsonNumbers, type Kind } from "./values.ts";
import { schemaOf, type EngineType } from "./engine-type.ts";
import { withSqlcommenter } from "./sqlcommenter.ts";
import type { JsonSchema } from "./engine-type.ts";
import type { SqlMode } from "./engine-core.ts";
import type { Checks } from "./checks.ts";
import type { Members, Structured } from "./rows.ts";
import type { RowBuilder } from "./row.ts";
import type { CodecRuntime, Libraries, Plan } from "./codecs.ts";

export type Dialect = "postgres" | "mysql" | "sqlite" | "clickhouse";

/**
 * pg's Pool, Client and PoolClient; PGlite and its transactions; Neon's Pool and Client;
 * pg-promise's database, tasks and transactions; @vercel/postgres; Drizzle's Postgres driver's
 * Pool and Connection: query(text, values) → { rows } (pg-promise resolves to the rows themselves,
 * and its types say any).
 */
export type PgQueryClient = {
  query(text: string, values?: any[]): PromiseLike<{ rows: unknown[] }>;
  /** mariadb's query() resolves to any as well; its escapeId() tells it apart. */
  escapeId?: never;
};
/**
 * Neon's neon() function from 1.0: query(text, values) → rows. Its transaction() tells it from
 * mysql2/promise, whose query(text, values) resolves to a [rows, fields] array as well.
 */
export type NeonHttpClient = {
  query(text: string, values?: any[]): PromiseLike<Record<string, unknown>[]>;
  transaction(...args: any[]): unknown;
};
/** postgres.js `sql`, its begin and savepoint transactions and reserved connections; Bun.sql, likewise: unsafe(text, params) → rows. */
export type UnsafeClient = {
  unsafe(text: string, params?: any[]): PromiseLike<unknown[]>;
};
/** Bun.sql on MySQL or SQLite; commitDistributed() tells it from postgres.js, which has unsafe() too. */
export type BunSqlClient = UnsafeClient & { commitDistributed(name: string): Promise<unknown> };
/** Neon's neon() function before 1.0, which is called with (text, params). */
export type CallableClient = (text: string, params?: any[]) => PromiseLike<unknown[] | { rows: unknown[] }>;
/** Slonik's pool, connections, transactions and savepoints, whose query() takes only its sql tag's queries. */
export type SlonikClient = { query(sql: any): PromiseLike<unknown>; maybeOneFirst(...args: any[]): unknown; anyFirst(...args: any[]): unknown };
/**
 * mysql2/promise Pool, Connection and PoolConnection (→ [rows, fields]); PlanetScale's Connection,
 * Client and transaction from 1.x (→ { rows }). PlanetScale 2.0 binds no parameters, so its
 * execute(query, options) is not one of these.
 */
export type MysqlExecuteClient = {
  execute(text: string, values?: any[]): PromiseLike<[unknown, unknown] | { rows: unknown[]; statement: string }>;
};
/** mysql2's callback Pool, Connection and PoolConnection, run through their promise() wrapper. */
export type Mysql2CallbackClient = { promise(): MysqlExecuteClient };
/** mariadb's Pool, Connection and PoolConnection: query(text, values) → rows, or an OK packet for a write. */
export type MariadbClient = {
  query(text: string, values?: any): Promise<unknown>;
  batch(text: string, values?: any): Promise<unknown>;
};
/** The same from mariadb's callback API (`mariadb/callback`): query(text, values, callback). */
export type MariadbCallbackClient = {
  query(text: string, values: any, callback: (error: Error | null, rows?: any) => void): void;
  batch(text: string, values: any, callback: (error: Error | null, result?: any) => void): void;
};
/**
 * better-sqlite3, node:sqlite's DatabaseSync, bun:sqlite's Database and the Database of Turso's
 * `@tursodatabase/database/compat`, which answer at once: `runSync` takes them and returns the
 * result itself, so it works inside their `db.transaction(fn)`, whose function must not await.
 */
export type SyncSqliteClient = {
  prepare(sql: string): { all(...params: any[]): unknown[] };
};
/**
 * The `oo1` Database of SQLite Wasm, `@sqlite.org/sqlite-wasm`, and the `OpfsDb` and
 * `OpfsSAHPoolDb` that keep one in the browser's origin-private file system, which answer at once:
 * `runSync` takes them. Their statements have no all(), and are read row by row with step().
 */
export type SqliteWasmDatabase = {
  prepare(sql: string): { bind(values: any[]): unknown; step(): boolean; get(row: any[]): unknown[]; getColumnNames(): string[]; finalize(): unknown };
  selectObjects(sql: string, bind?: any): unknown[];
};
/**
 * sql.js's Database, which answers at once: `runSync` takes it. Its statements have no all(), and
 * are read row by row with step(), every INTEGER as a BigInt.
 */
export type SqlJsDatabase = {
  prepare(sql: string): { bind(values?: any): boolean; step(): boolean; get(params?: any, config?: { useBigInt?: boolean }): unknown[]; getColumnNames(): string[]; free(): boolean };
  getRowsModified(): number;
};
/**
 * A SQLite-backed Durable Object's SQL storage, `ctx.storage.sql`: exec(sql, ...values) answers at
 * once with a cursor, so `runSync` takes it, inside `ctx.storage.transactionSync(fn)` too.
 */
export type DurableObjectSql = {
  exec(query: string, ...bindings: any[]): { toArray(): unknown[] };
  readonly databaseSize: number;
};
/** A Durable Object's storage, `ctx.storage`, which `drizzle-orm/durable-sqlite` keeps: `run` takes its `sql`. */
export type DurableObjectStorage = { readonly sql: DurableObjectSql };
/** Cloudflare D1's database and sessions: prepare(sql).bind(...values).all() → { results }. */
export type D1Client = {
  prepare(sql: string): { bind(...values: any[]): { all(): Promise<{ results: unknown[] }> } };
};
/** @libsql/client's Client and Transaction: execute({ sql, args }) → { rows }. */
export type LibsqlClient = {
  execute(statement: { sql: string; args: any[] }): Promise<{ rows: unknown[] }>;
  /** mariadb's execute() takes an object with sql and resolves to any; executeMultiple() tells libSQL apart. */
  executeMultiple(sql: string): Promise<unknown>;
};
/**
 * Turso's new engine, `@tursodatabase/database`: the Database `connect()` resolves to and the
 * Transaction `transactionAsync()` hands its callback, whose statements answer through a promise:
 * prepare(sql).all(...values) → rows. Its types say prepare() returns a promise of the statement
 * from 0.6, and the statement itself before; it returns the statement itself in every release.
 */
export type TursoClient = {
  prepare(sql: string): TursoStatement | PromiseLike<TursoStatement>;
  /** postgres.js's transaction has a prepare(name) of any type too; exec() tells Turso's apart. */
  exec(sql: string): Promise<unknown>;
};
type TursoStatement = { all(...params: any[]): Promise<unknown[]> };
/** The sqlite3 package's callback Database: all(sql, params, callback). */
export type Sqlite3Client = {
  all(sql: string, params: any[], callback: (error: Error | null, rows: any[]) => void): unknown;
};
/** The sqlite package's promise Database: all(sql, params) → rows. */
export type SqlitePromiseClient = {
  all(sql: string, params?: any): Promise<unknown[]>;
  getDatabaseInstance(): unknown;
};

/**
 * A Drizzle database over one of these clients (`drizzle(pool)`, whose `$client` is the pool):
 * `run` runs on the driver client underneath, so the values are what Reylo decodes on that driver.
 */
export type DrizzleDatabase<C> = { readonly $client: C };
/**
 * A transaction or savepoint a Drizzle database hands out (`db.transaction(async (tx) => …)`),
 * which `run` runs on through the transaction's own connection. Each dialect's is known by what
 * only it has: `setTransaction()` on Postgres, `get()` and `all()` on SQLite, neither on MySQL.
 */
export type DrizzlePgTransaction = { rollback(): never; setTransaction(config: any): Promise<unknown> };
export type DrizzleMysqlTransaction = { rollback(): never; execute(query: any): unknown; setTransaction?: never; get?: never };
export type DrizzleSqliteTransaction = { rollback(): never; get(query: any): unknown; all(query: any): unknown };
/** A transaction of a synchronous SQLite driver's Drizzle database, whose `all()` returns the rows themselves. */
export type DrizzleSyncSqliteTransaction = { rollback(): never; get(query: any): unknown; all(query: any): unknown[] };
/**
 * A Knex instance, or a transaction or savepoint it hands out (`knex.transaction(async (trx) => …)`):
 * `run` runs on a connection its `client` acquires from the pool and releases after, which inside
 * a transaction is the transaction's own, so the values are what Reylo decodes on that driver
 * (`pg`, `mysql2`, `better-sqlite3` or `sqlite3`). Knex types `client` as any, so the type cannot
 * say which database an instance is for: a statement for another one is refused when it runs.
 */
export type KnexClient = {
  readonly client: { acquireConnection(): Promise<unknown>; releaseConnection(connection: any): unknown };
  raw(...args: any[]): unknown;
  transaction(...args: any[]): unknown;
};
/** Every Postgres client `run` takes, or a transaction, savepoint or connection taken from one. */
export type PostgresClient = PgQueryClient | NeonHttpClient | UnsafeClient | CallableClient | SlonikClient | DrizzleDatabase<PgQueryClient | NeonHttpClient | UnsafeClient | CallableClient> | DrizzlePgTransaction | KnexClient;
/** Every MySQL client `run` takes, or a transaction or connection taken from one. */
export type MysqlClient = MysqlExecuteClient | Mysql2CallbackClient | MariadbClient | MariadbCallbackClient | BunSqlClient | DrizzleDatabase<MysqlExecuteClient | Mysql2CallbackClient | MariadbClient | BunSqlClient> | DrizzleMysqlTransaction | KnexClient;
/**
 * `@clickhouse/client`'s and `@clickhouse/client-web`'s client: exec() runs a statement and hands
 * back the response's bytes, which Reylo reads itself.
 */
export type ClickhouseClient = { exec(params: any): PromiseLike<unknown>; query(params: any): PromiseLike<unknown>; insert(params: any): PromiseLike<unknown>; command(params: any): PromiseLike<unknown> };
/** Every SQLite client `run` takes, or a transaction taken from one. */
export type SqliteClient = SyncSqliteClient | SqliteWasmDatabase | SqlJsDatabase | DurableObjectSql | D1Client | LibsqlClient | TursoClient | Sqlite3Client | SqlitePromiseClient | BunSqlClient | DrizzleDatabase<SyncSqliteClient | SqlJsDatabase | DurableObjectStorage | D1Client | LibsqlClient | BunSqlClient> | DrizzleSqliteTransaction | KnexClient;

// Each dialect's clients are one named union, so a hover over `run` reads
// `run(client: PostgresClient, …)` rather than listing every client's shape.
export type ClientFor<D extends Dialect> = D extends "postgres" ? PostgresClient : D extends "mysql" ? MysqlClient : D extends "clickhouse" ? ClickhouseClient : SqliteClient;

/** The clients `runSync` takes: the synchronous SQLite drivers, and none for Postgres or MySQL. */
export type SyncClientFor<D extends Dialect> = D extends "sqlite" ? SyncSqliteClient | SqliteWasmDatabase | SqlJsDatabase | DurableObjectSql | DrizzleDatabase<SyncSqliteClient | SqlJsDatabase | DurableObjectStorage> | DrizzleSyncSqliteTransaction : never;

export type QueryModule<D extends Dialect = Dialect> = {
  readonly dialect: D;
  /** The statement as written, with Reylo's placeholders (`:id`) left in place: the query's `raw`. */
  readonly sql: string;
  /** Parameter names in placeholder order, so an object of params binds positionally. */
  readonly params: readonly string[];
  /** The statement as the engine compiled it for the database, when a build step ran: the query's `sql`. A query without it is taken as written (`compile` below). */
  readonly text?: string | undefined;
  /** The binding order for `text`: one name per placeholder of the rewritten statement. */
  readonly order?: readonly string[] | undefined;
  /** Each placeholder's place in `text`, as the engine found it: where Reylo splits the statement for a driver that takes it in pieces (Slonik's tag) or writes the values into it (mysql2 on a server that refuses prepared statements, src/decode-mysql.ts). A ClickHouse placeholder carries its parameter's type, which Reylo reads at its mark to write the value (src/decode-clickhouse.ts). A build step writes them for Postgres, MySQL and ClickHouse; those two drivers, and a ClickHouse statement with parameters, refuse a query without them. */
  readonly marks?: Marks | undefined;
  /** How the MySQL session the statement was built on reads strings, where another `sql_mode` would read the statement otherwise: Reylo writes values into its text only on a session that reads it the same way (src/decode-mysql.ts). */
  readonly sqlMode?: SqlMode | undefined;
  /** What the analysis found of each parameter, by name (every build step puts it in the module). */
  readonly inputs?: Inputs | undefined;
  /** Each output column's name and kind (src/values.ts), in order: what the run time gives each value (every build step puts it in the module). */
  readonly columns?: Columns | undefined;
  /** Where each Postgres JSON column, by position beside `columns`, holds a float or a decimal that may be NaN or an infinity, which the run time gives as that number, or a Postgres array it holds to one level (src/values.ts, `numbersInJson`); `null` for a column that holds neither. */
  readonly nonFinite?: readonly (JsonNumbers | null)[] | undefined;
  /** Each row as an object of the columns' names, one shape for every row (src/row.ts): every build step puts it in the module, beside `columns`. */
  readonly row?: RowBuilder | undefined;
  /** The result key: output columns, by name, that identify each row of the result, where the analysis proves one. */
  readonly key?: readonly string[] | undefined;
  /** Whether the statement's ORDER BY orders its rows totally, where the analysis proves it. */
  readonly ordered?: boolean | undefined;
  /** How many rows the statement returns, which a query's Standard Query says beside its `decode`. */
  readonly cardinality?: "many" | "one" | "maybeOne" | "none" | undefined;
  /** Whether the result is its one column's values, a column the statement gives no name, so that whatever name the database makes up for it says nothing. */
  readonly value?: boolean | undefined;
  /** The columns that hold a family the project file's `js.codecs` gives another type, by name, each with where in its value the codecs stand (src/codecs.ts). */
  readonly codecs?: Readonly<Record<string, Plan>> | undefined;
  /** The classes the codecs build values with, which the module imports from the app's install (src/codecs.ts). */
  readonly libraries?: Libraries | undefined;
  /** `js.codecs` at run time (src/codecs.ts), which a build step passes where the statement holds a value a codec gives another type. */
  readonly codecRuntime?: CodecRuntime | undefined;
  /** Each structured parameter in insert position, with the lists of `text` the run time writes again for the keys a call carries. */
  readonly structured?: readonly Structured[] | undefined;
  /** The `.sql` file the module was built from, relative to the project's root, for messages. */
  readonly source?: string | undefined;
  /** Whether a Postgres array column may hold nested arrays, as `allow_nested_arrays: true` says: none is refused. */
  readonly nestedArrays?: boolean | undefined;
  /** Whether the statement Reylo sends carries a sqlcommenter comment naming `source` (src/sqlcommenter.ts); `text`, the query's `sql`, leaves it out. */
  readonly sqlcommenter?: boolean | undefined;
  /** The checks of a typed JSON, structured or cast parameter (src/checks.ts), which a build step passes where the statement has one. */
  readonly checks?: Checks | undefined;
  /** The dialect's half of the run time, which its define function passes (src/index.ts). */
  readonly runtime: Runtime<D>;
};

/**
 * What the run time does differently on each database, each in its database's own module: running
 * a statement on that database's drivers (src/decode.ts for Postgres, decode-mysql.ts,
 * decode-sqlite.ts), reading their errors (src/query-error.ts) and decoding a
 * Standard Query's raw values (src/standard-query.ts). A module a build step writes calls its
 * dialect's define function, which alone names its runtime, so a bundle holds the runtimes of the
 * dialects its queries are written for and no other.
 */
export type Runtime<D extends Dialect = Dialect> = {
  readonly dialect: D;
  /** The rows of the bound statement on `c`, or undefined where `c` is none of this database's clients that answer through a promise. */
  run(c: AnyClient, query: QueryModule<D>, bound: Bound): Promise<unknown[] | undefined>;
  /** SQLite's synchronous clients (better-sqlite3, node:sqlite, bun:sqlite, Turso's compat Database, SQLite Wasm's and sql.js's Database, a Durable Object's SQL storage): the rows at once, or a promise where a statement answers with one. */
  runSync?(c: AnyClient, query: QueryModule<D>, bound: Bound): unknown[] | Promise<unknown[]>;
  /** Whether `c` is one of the clients `runSync` takes, which `.run()` hands to it too. */
  answersAtOnce?(c: AnyClient): boolean;
  /** `.runSync()`'s whole path (`executeSync`), for the database whose clients answer at once: any other database's refuses (`refuseSync`). */
  executeSync?(client: SyncClientFor<D>, query: QueryModule<D>, params: ParamsInput): unknown[];
  /** The clients Drizzle and Knex wrap, for the databases they run on (`wrappers`). */
  readonly wrappers?: Wrappers;
  /** What `.run()` checks on the client before the statement runs (Postgres's `lc_monetary`, for a `money` parameter). */
  before?(c: AnyClient, query: QueryModule<D>): Promise<void>;
  /** The rows of an asynchronous client as `.run()` returns them, where the driver's need more (Postgres's numbers inside JSON, and its arrays' dimensions). */
  after?(query: QueryModule<D>, rows: unknown[]): unknown[];
  /** A parameter's value of the engine's type `type`, checked, as the database's drivers bind it (`statement`): neither is null or undefined. */
  encode(type: EngineType, v: unknown, name: string): unknown;
  /** A bound value as the database reads it with no declared type, which a Standard Query's `encode` returns (Postgres's text, `rawStatement`); every other database's as it is bound. */
  rawValue?(query: QueryModule<D>, name: string, v: unknown): unknown;
  /** The clients `.run()` takes, in the refusal of any other (`unsupported`). */
  readonly clients: string;
  /** The database's error `error` is, or undefined where it is another (`.try()`). */
  readError(error: unknown): QueryError<D> | undefined | Promise<QueryError<D> | undefined>;
  /** The same at once, for the synchronous clients (`.trySync()`). */
  readErrorSync?(error: unknown): QueryError<D> | undefined;
  /** A column's raw value as `.run()` gives it, as a Standard Query's `decode` maps it (src/standard-query.ts). */
  decoder(name: string, kind: Kind, delim: string | undefined, nonFinite: JsonNumbers | undefined, nestedArrays: boolean | undefined): (v: never) => unknown;
};

/** Placeholders' places in a statement: `[start, end]` for each, at JavaScript's offsets, in text order. */
export type Marks = readonly (readonly [number, number])[];

/** The statement as Reylo sends it, its values in binding order, the parameter each binds, and each placeholder's place in the text sent, where the module carries them. */
export type Bound = { text: string; values: unknown[]; names: readonly string[]; marks: Marks | undefined };

/**
 * Each parameter by name: `type` is the engine's type for it. The run time reads it where a
 * driver binds a value of one type wrong (an array to a SQL array on postgres.js and Bun.sql, to
 * JSON on pg); a query without it, defined by hand or built before 2026-10-12, binds every value
 * as it is. A typed JSON parameter (wiki/typed-json-params.md) carries its schema beside it, and
 * a parameter cast with `::` on MySQL or SQLite the type as written (src/rows.ts, `checkCast`).
 * `nullable` is false where the statement takes no null for the parameter, which the run time
 * then refuses, given or left out, except that a JSON parameter's null there is JSON's null; a
 * query defined by hand without it binds what it is given.
 */
export type Inputs = Readonly<Record<string, { readonly type: EngineType; readonly schema?: JsonSchema; readonly cast?: string; readonly members?: Members; readonly delimiter?: string; readonly nullable?: boolean; readonly codec?: Plan }>>;

export type AnyClient = Record<string, any> & ((...args: any[]) => any);

/**
 * The parameters: an object keyed by name, or by number for positional placeholders (`$1` and
 * `?` give `{ 1: ..., 2: ... }`, and `params` is `["1", "2"]`). An array is refused, since
 * 2026-10-20 (wiki/design.md §9).
 */
export type ParamsInput = Record<string, unknown>;

/**
 * Runs the statement and returns its rows (an empty array for a statement that returns none).
 * On a synchronous SQLite client the rows come back at once and an error is thrown; on every
 * other client they come back as a promise, which rejects with the driver's own error.
 */
export function execute<D extends Dialect>(
  client: ClientFor<D>,
  query: QueryModule<D>,
  params: ParamsInput = {},
): unknown[] | Promise<unknown[]> {
  const { runtime } = query;
  const c = runtime.wrappers?.underneath(client as AnyClient) ?? (client as AnyClient);
  const lent = runtime.wrappers?.knex(c, query.dialect, (connection) => execute(connection as ClientFor<D>, query, params));
  if (lent) return lent;
  if (runtime.answersAtOnce?.(c)) return withCodecs(query, syncRows(c, query, params));
  const rows = executeAsync(c, query, params);
  const after = runtime.after;
  return withCodecs(query, after ? rows.then((r) => after(query, r)) : rows);
}

/** The rows with the codecs of the module's columns applied where they stand (src/codecs.ts): one pass, where a column holds one. */
function withCodecs(query: QueryModule, rows: unknown[] | Promise<unknown[]>): unknown[] | Promise<unknown[]> {
  const codecs = query.codecs;
  if (codecs === undefined || Object.keys(codecs).length === 0) return rows;
  const run = codecsOf(query);
  return Array.isArray(rows) ? run.decodeCodecs(rows, codecs, query.libraries) : rows.then((r) => run.decodeCodecs(r, codecs, query.libraries));
}

/** `js.codecs` at run time (src/codecs.ts): every build step passes it where the statement holds a value a codec gives another type. */
export function codecsOf(query: QueryModule): CodecRuntime {
  if (query.codecRuntime) return query.codecRuntime;
  throw new TypeError(`${query.source ? `${query.source}: ` : ""}This statement holds a value js.codecs gives another type, which the run time converts with Reylo's codecRuntime, which a module a build step writes passes: build a module an earlier reylo wrote again (reylo generate typescript writes the files it wrote). A query defined by hand with its dialect's define function passes it too: define${query.dialect[0]!.toUpperCase()}${query.dialect.slice(1)}Query({ …, codecRuntime }) with codecRuntime from "reylo".`);
}

/**
 * The driver client a Drizzle database, transaction or savepoint runs on, which its session
 * keeps on `session.client` (the pool for a database, the transaction's own connection inside
 * one), or on `session.tx` inside a libSQL transaction, whose `client` stays the base client;
 * any other client as it is.
 */
function underneath(c: AnyClient): AnyClient {
  const o = c as unknown as { select?: unknown; session?: { client?: unknown; tx?: unknown; dialect?: unknown } } | null | undefined;
  const session = o != null && typeof o === "object" && typeof o.select === "function" ? o.session : undefined;
  if (session == null || typeof session !== "object" || session.dialect == null) return c;
  const client = ((session.tx ?? session.client) as AnyClient | undefined) ?? c;
  // drizzle-orm/durable-sqlite keeps the Durable Object's storage, whose SQL storage is its sql.
  const sql = (client as { sql?: AnyClient }).sql;
  return isDurableObjectSql(sql) ? sql! : client;
}

// A Knex instance, transaction or savepoint: a function with raw() and transaction(), whose client
// hands out the driver's connections. Checked before every other client, since the instance is a
// function, as Neon's neon() is, and calling it starts a query builder on a table of that name.
function isKnex(c: AnyClient | null | undefined): boolean {
  if (typeof c !== "function" || typeof c.raw !== "function" || typeof c.transaction !== "function") return false;
  const client = (c as { client?: { acquireConnection?: unknown; releaseConnection?: unknown } }).client;
  return typeof client?.acquireConnection === "function" && typeof client.releaseConnection === "function";
}

// The drivers Knex names on its client, `client.driverName`, whose connections Reylo reads, each with
// its database. Any other (pgnative, mysql, cockroachdb, pg-redshift, mssql, oracledb) is refused.
const knexDrivers: Readonly<Record<string, Dialect>> = { pg: "postgres", mysql2: "mysql", mariadb: "mysql", "better-sqlite3": "sqlite", sqlite3: "sqlite" };

/**
 * Runs on a connection a Knex instance or transaction hands out: one its pool lends, given back
 * after, or the transaction's own, whose release does nothing. A transaction that has committed or
 * rolled back has given its connection back to the pool, so it is refused, as Knex refuses it. Its
 * type cannot say which database an instance is for, so one for another database is refused here.
 */
async function onKnexConnection(knex: AnyClient, dialect: Dialect, run: (connection: AnyClient) => unknown[] | Promise<unknown[]>): Promise<unknown[]> {
  const driver: unknown = knex.client.driverName;
  const database = typeof driver === "string" && Object.hasOwn(knexDrivers, driver) ? knexDrivers[driver] : undefined;
  if (typeof driver === "string" && database === undefined) throw new TypeError(`reylo cannot run on a Knex instance over ${driver}: it reads the connections of ${Object.keys(knexDrivers).join(", ")}.`);
  if (database !== undefined && database !== dialect) throw new TypeError(`reylo cannot run a ${dialect} statement on a Knex instance for ${driver}, a ${database} driver: pass a Knex instance for ${dialect}.`);
  if (typeof knex.isCompleted === "function" && knex.isCompleted()) throw new Error("reylo cannot run a statement on a Knex transaction that has already committed or rolled back: its connection is back in the pool.");
  const connection = await knex.client.acquireConnection();
  try {
    return await run(connection);
  } finally {
    await knex.client.releaseConnection(connection);
  }
}

/** A Drizzle database's client and a Knex instance's connections, for the databases Drizzle and Knex run on (`Runtime`), so that a bundle of another holds neither. */
export type Wrappers = {
  /** The driver's client a Drizzle database runs on (`underneath`), else `c` itself. */
  underneath(c: AnyClient): AnyClient;
  /** The rows of `run` on a connection a Knex instance lends, or undefined where `c` is none. */
  knex(c: AnyClient, dialect: Dialect, run: (connection: AnyClient) => unknown[] | Promise<unknown[]>): Promise<unknown[]> | undefined;
};
export const wrappers: Wrappers = { underneath, knex: (c, dialect, run) => (isKnex(c) ? onKnexConnection(c, dialect, run) : undefined) };

/** The synchronous path alone, for `runSync`: any other client is refused before anything runs. */
export function executeSync<D extends Dialect>(client: SyncClientFor<D>, query: QueryModule<D>, params: ParamsInput = {}): unknown[] {
  const c = query.runtime.wrappers?.underneath(client as unknown as AnyClient) ?? (client as unknown as AnyClient);
  if (!query.runtime.runSync) throw refuseSync(`a ${query.dialect} statement has none`);
  if (!query.runtime.answersAtOnce?.(c)) throw refuseSync("this client answers through a promise");
  const rows = syncRows(c, query, params);
  if (Array.isArray(rows)) return withCodecs(query, rows) as unknown[];
  rows.catch(() => {});
  throw refuseSync("this client's statement answered with a promise");
}

/** `.runSync()` on a client that does not answer at once, or a statement of a database that has none. */
export function refuseSync(why: string): TypeError {
  return new TypeError(`runSync takes a synchronous SQLite client (better-sqlite3, node:sqlite, bun:sqlite, the Database of @tursodatabase/database/compat, an oo1 Database of @sqlite.org/sqlite-wasm, sql.js's Database or a Durable Object's ctx.storage.sql); ${why}. run takes every client.`);
}

/** The rows of a client that answers at once (`Runtime`, `answersAtOnce`). */
function syncRows<D extends Dialect>(c: AnyClient, query: QueryModule<D>, params: ParamsInput): unknown[] | Promise<unknown[]> {
  checkParams(query, params);
  return query.runtime.runSync!(c, query, statement(query, params));
}

// A SQLite-backed Durable Object's `ctx.storage.sql`: exec() and databaseSize, and no prepare().
// sqlite3's Database has exec() too, beside all().
export function isDurableObjectSql(c: AnyClient | null | undefined): boolean {
  return c != null && typeof c.exec === "function" && typeof c.prepare !== "function" && typeof c.all !== "function" && "databaseSize" in c;
}

async function executeAsync(c: AnyClient | null | undefined, query: QueryModule, params: ParamsInput): Promise<unknown[]> {
  if (c == null || (typeof c !== "object" && typeof c !== "function")) throw unsupported(c, query);
  // Effect SQL's client has an unsafe() whose statement is an Effect, which an await never runs:
  // taken for Bun.sql's on SQLite, a read gave no rows and a write ran nothing. Effect 4 brands
  // the client with a string, Effect 3's @effect/sql with a symbol.
  if ("~effect/sql/SqlClient" in c || Symbol.for("@effect/sql/SqlClient") in c) throw new TypeError(`reylo cannot run a statement on an Effect SQL client: .run() takes a driver's own client, and Effect SQL's statements are Effects, which only Effect's runtime runs.`);
  checkParams(query, params);
  await query.runtime.before?.(c, query);
  const rows = await query.runtime.run(c, query, statement(query, params));
  if (rows === undefined) throw unsupported(c, query);
  return rows;
}

// The parameters are an object, refused before anything reaches the database otherwise: an
// array, which `run` took in the order of `params` from 2026-10-03, was dropped on 2026-10-20
// (wiki/design.md §9), so the call says which key each value binds.
function checkParams(query: QueryModule, input: unknown): asserts input is Record<string, unknown> {
  if (input !== null && typeof input === "object" && !Array.isArray(input)) return;
  const keys = query.params.map((name) => `${/^(?:[A-Za-z_$][\w$]*|\d+)$/.test(name) ? name : JSON.stringify(name)}: ...`);
  const takes = keys.length ? `This statement takes { ${keys.join(", ")} }.` : "This statement takes none: leave them out.";
  throw new ParameterError(`The parameters must be an object keyed by parameter name, or by number for positional placeholders ({ 1: ..., 2: ... }); got ${Array.isArray(input) ? "an array" : String(input)}. ${takes}`);
}

// The rows are bound positionally in every dialect. A value left out binds NULL, as pg,
// PGlite, better-sqlite3 and Bun.sql already do; postgres.js, mysql2, node:sqlite and libSQL
// would reject it instead.
function statement(query: QueryModule, params: ParamsInput) {
  // The build step already compiled the statement; only the values need ordering.
  const compiled = compile(query);
  const bound: Bound = { text: compiled.text, values: compiled.order.map((name) => value(params, name)), names: compiled.order, marks: compiled.marks };
  bound.names.forEach((name, i) => {
    const input = query.inputs?.[name];
    const type = input?.type;
    // A typed JSON parameter (`:p::jsonb('…')`): a value left out takes the schema's default, a
    // key under `properties` too, and the value is checked against the schema before the
    // statement runs (wiki/typed-json-params.md). Null is SQL NULL where the statement takes SQL
    // NULL for the parameter, and elsewhere JSON's null, where the schema admits it; a value left
    // out there is refused.
    const schema = input?.schema;
    if (schema !== undefined) {
      let v = given(params, name);
      const { cloneJson, validate, withDefaults } = checksFor(query, name);
      if (v === undefined && schema !== null && typeof schema === "object" && "default" in schema) v = cloneJson(schema.default);
      if (v !== undefined && v !== null) {
        v = withDefaults(schema, v);
        const failure = validate(schema, v);
        if (failure) throw new ParameterError(`Parameter "${name}"${failure.path ? ` at ${failure.path}` : ""} ${failure.message}.`);
      } else if (input?.nullable === false) {
        if (v === undefined || validate(schema, null)) refuseNull(name, v);
        v = JSON_NULL;
      }
      bound.values[i] = v === undefined ? null : v;
      return;
    }
    // A value of a family the project file's `js.codecs` gives another type takes that type alone,
    // wherever it stands in the parameter, a structured parameter's members among them, and is
    // bound as the text or the bytes its type reads (src/codecs.ts).
    if (input?.codec !== undefined) bound.values[i] = codecsOf(query).encodePlan(input.codec, bound.values[i], name, query.dialect, query.libraries);
    // A structured parameter is checked against its column list and written as the JSON the
    // statement reads; in an insert, its lists name only the keys the call carries. It refuses
    // null itself, saying what to pass.
    if (type !== undefined && input?.members !== undefined) {
      const structured = query.structured?.find((s) => s.name === name);
      const { prepareRows, rowsOf, variant } = checksFor(query, name);
      const rows = rowsOf(type, input.members);
      const prepared = prepareRows(name, rows, bound.values[i], query.dialect, structured !== undefined);
      bound.values[i] = prepared.value;
      if (structured) ({ text: bound.text, marks: bound.marks } = variant(bound.text, bound.marks, structured, rows.fields, prepared.present));
      return;
    }
    // A parameter the statement takes no null for, which its types rely on (a column it alone
    // fills, an expression of it typed non-null), refuses null and refuses to be left out, which
    // would bind NULL: exactly where its declared type takes no null (src/render.ts,
    // `renderParamType`). One typed `unknown` is declared `unknown` and takes anything. A JSON one
    // takes JSON's null there instead, unless the schema its type carries refuses null, and is
    // refused only when left out.
    let jsonNull = false;
    if (input?.nullable === false && bound.values[i] === null) {
      if (type?.kind === "json" && given(params, name) !== undefined && !refusesNull(query, type)) jsonNull = true;
      else if (type?.kind === "json" || refusesNull(query, type)) refuseNull(name, given(params, name));
    }
    // An array parameter takes no NULL element, as its type says (`:ids::int[]` is `number[]`):
    // a NULL in `= any(:ids)` matches nothing. An array inside a
    // structured parameter's member keeps its NULL elements (src/rows.ts).
    // A ClickHouse array's type says whether an element may be NULL, and its binder holds each to it.
    if (type?.kind === "array" && Array.isArray(bound.values[i]) && query.dialect !== "clickhouse") {
      const at = nullElement(bound.values[i] as unknown[]);
      if (at !== undefined) throw new ParameterError(`Parameter "${name}" is an array whose elements may not be null, and the element at ${at} is. Leave it out of the array.`);
    }
    // A `::type` written on MySQL or SQLite, whose cast turns a value it cannot read into 0 or
    // NULL: the value is checked against the type as written.
    const cast = query.inputs?.[name]?.cast;
    if (cast !== undefined && type !== undefined) checksFor(query, name).checkCast(name, cast, type, bound.values[i], query.dialect);
    checkTime(type, name, bound.values[i], query.dialect);
    checkBinary(type, name, bound.values[i], query.dialect);
    const v = bound.values[i];
    bound.values[i] = jsonNull ? JSON_NULL : type === undefined || v == null ? v : query.runtime.encode(type, v, name);
  });
  // Every MySQL and SQLite driver binds JSON text as text, which the database reads as JSON (a
  // structured parameter's is written already).
  if (query.dialect !== "postgres") bound.values = bound.values.map((v, i) => (isJsonValue(query, bound.names[i]!, v) && query.inputs?.[bound.names[i]!]?.members === undefined ? jsonText(v) : v));
  // SQLite holds an INTEGER in 64 bits, so a BigInt outside them has no exact form there.
  // better-sqlite3, node:sqlite, libSQL, Turso's engine and SQLite Wasm refuse one, but bun:sqlite
  // and Bun.sql keep its low 64 bits where the statement reads numbers (2^63 was stored as -2^63,
  // and 2^64 + 5 as 5), and sqlite3 binds any BigInt as NULL.
  if (query.dialect === "sqlite")
    bound.values.forEach((v, i) => {
      if (typeof v === "bigint" && (v >= 2n ** 63n || v < -(2n ** 63n))) throw new ParameterError(`Parameter "${bound.names[i]}" is ${v}, outside the 64-bit integers SQLite holds (-2^63 to 2^63 - 1), so no SQLite driver binds it exactly.`);
    });
  // The comment naming the file goes on last, after a structured parameter's lists are written. It
  // ends the statement on Postgres and MySQL, so every mark keeps its place; on ClickHouse it opens
  // the statement, and every mark moves by its length.
  const sent = sentText(query, bound.text);
  const moved = sent.length - bound.text.length;
  if (query.dialect === "clickhouse" && moved > 0 && bound.marks !== undefined) bound.marks = bound.marks.map(([start, end]) => [start + moved, end + moved] as const);
  bound.text = sent;
  return bound;
}

/** `text` as Reylo sends it: with the comment naming the query's file, where the module asks for one (src/sqlcommenter.ts). */
export function sentText(query: QueryModule, text: string): string {
  return query.sqlcommenter && query.source ? withSqlcommenter(text, query.source, query.dialect) : text;
}

/**
 * The statement and its values in the database's raw form, which a Standard Query's `encode`
 * returns (src/standard-query.ts), bound as `.run()` binds them, its checks included: on Postgres
 * each value's text, which a driver binds with no declared type, so the server reads it as the
 * parameter's type; on MySQL and SQLite each value as a driver binds it. ClickHouse has no such
 * form, and a Standard Query on it binds nothing (src/standard-query.ts).
 */
export function rawStatement(query: QueryModule, params: unknown): { text: string; values: unknown[] } {
  checkParams(query, params);
  const bound = statement(query, params);
  const { text, values, names } = bound;
  const raw = query.runtime.rawValue;
  return raw ? { text, values: values.map((v, i) => raw(query, names[i]!, v)) } : { text, values };
}

/** A bound value as the text Postgres's input function reads, as `.run()` sends it through pg. */
export function postgresText(query: QueryModule, name: string, v: unknown): string | null {
  if (v == null) return null;
  const t = query.inputs?.[name]?.type;
  if (isJsonValue(query, name, v)) return jsonText(v);
  if (Array.isArray(v) && t?.kind === "array") return arrayLiteral(t.element.type.kind === "json" ? v.map((e) => (e == null ? e : JSON.stringify(e))) : v, query.inputs?.[name]?.delimiter);
  if (Array.isArray(v)) return arrayLiteral(v);
  if (v instanceof Uint8Array) return asByteaHex(v) as string;
  if (typeof v === "boolean") return v ? "t" : "f";
  if (v instanceof Date) return dateText(v);
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** The checks a parameter needs (src/checks.ts): every build step passes them where its statement has such a parameter. */
function checksFor(query: QueryModule, name: string): Checks {
  if (query.checks) return query.checks;
  throw new ParameterError(`Parameter "${name}" is a typed JSON, structured or cast parameter, which the run time checks with Reylo's checks; a query defined by hand passes them: defineQuery({ …, checks }) with checks from "reylo".`);
}

/** Where an array, or an array inside it (a multidimensional one), holds null or undefined: `[2]`, `[1][0]`. */
function nullElement(array: readonly unknown[], path = ""): string | undefined {
  for (let i = 0; i < array.length; i++) {
    const e = array[i];
    if (e == null) return `${path}[${i}]`;
    if (Array.isArray(e)) {
      const inner = nullElement(e, `${path}[${i}]`);
      if (inner !== undefined) return inner;
    }
  }
  return undefined;
}

/** Whether a parameter the statement takes no null for is declared with a type that takes no null: for a JSON one, whether the schema its type carries refuses JSON's null too. */
function refusesNull(query: QueryModule, type: EngineType | undefined): boolean {
  if (type === undefined || type.kind === "unknown") return false;
  if (type.kind !== "json") return true;
  const schema = schemaOf(type);
  return schema !== undefined && query.checks !== undefined && query.checks.validate(schema, null) !== undefined;
}

/** A parameter the statement takes no null for, given null or left out. */
function refuseNull(name: string, v: unknown): never {
  throw new ParameterError(`Parameter "${name}" ${v === undefined ? "is missing" : "is null"}, and the statement takes no null for it: pass a value.`);
}

function value(params: Record<string, unknown>, name: string) {
  const v = given(params, name);
  return v === undefined ? null : v;
}

// A JSON parameter's value is a JavaScript value, which reaches the database as its JSON text
// (JSON.stringify), so a string is a JSON string on every driver; null is SQL NULL where the
// statement takes SQL NULL for the parameter, and JSON's null elsewhere (`JSON_NULL`). Each driver
// left to itself did something else (wiki/driver-decoding.md, 2026-10-12): pg wrote an array as
// an array literal and a string as raw JSON text, mariadb expanded an array into a list, the
// SQLite drivers refused an object, and mysql2 bound 5 as 5.0.
export function isJsonValue(query: QueryModule, name: string, v: unknown): boolean {
  const input = query.inputs?.[name];
  return v != null && (input?.type.kind === "json" || input?.members !== undefined);
}

/**
 * JSON's null, which a JSON parameter's null stands for where the statement takes no SQL NULL for
 * it, as the engine reports it: a NOT NULL `jsonb` column holds JSON's null and refuses SQL NULL.
 * A `coalesce` or a null test of the parameter takes no SQL NULL either, as for every other type,
 * so `coalesce(:p::jsonb, col)` given null keeps JSON's null rather than `col`; a cast to
 * `nullable(jsonb)` is what passes SQL NULL there. It stays apart from SQL NULL until each driver's
 * binding writes it as the JSON text `null`.
 */
const JSON_NULL: unique symbol = Symbol("JSON null");

/** A JSON parameter's value as JSON: `JSON_NULL` as null. */
export const jsonValue = (v: unknown) => (v === JSON_NULL ? null : v);

/** A JSON parameter's value as the JSON text a driver binds. */
export const jsonText = (v: unknown) => JSON.stringify(jsonValue(v));

// A date or time value's type says whether it has a zone (src/render.ts),
// and a value that would be read in a zone nobody chose is refused before the
// statement runs. A `timestamp` or a `date` (MySQL's DATETIME and TIMESTAMP are the engine's
// `timestamp`) takes no Date: pg and mysql2 wrote its wall-clock time in the app's zone,
// postgres.js in the server's, PGlite and Bun.sql in UTC, and Bun.sql's `date` failed in every
// zone. A `timestamptz` takes a string that names its zone, which is what `run` returns for one:
// without a zone the server reads the text in the session's TimeZone, which the driver or the
// server sets, and a Date is refused too, as every other type's value is but a string,
// with the call that writes it as one.

export const isTime = (type: EngineType | undefined, dialect: Dialect) => type !== undefined && columnKind(type, dialect).split("|").every((part) => part === "timestamp" || part === "timestamptz" || part === "date");

function checkTime(type: EngineType | undefined, name: string, v: unknown, dialect: Dialect): void {
  if (type === undefined || v == null) return;
  if (type.kind === "array") {
    if (Array.isArray(v)) for (const e of v) checkTime(Array.isArray(e) ? type : type.element.type, name, e, dialect);
    return;
  }
  const kind = columnKind(type, dialect);
  if (kind === "timestamptz") {
    if (v instanceof Date) {
      throw new ParameterError(`Parameter "${name}" is a timestamptz, which takes the instant as a string that names its zone, as \`.run()\` returns one, not a Date: pass date.toISOString() ("2026-10-12T19:06:48.282Z").`);
    }
    if (typeof v === "string" && !zoned.test(v.trim())) {
      throw new ParameterError(`Parameter "${name}" is a timestamptz, an instant, so a string for it names its zone ("2026-10-12T19:06:48.282Z", "2026-10-12 19:06:48+09", "2026-10-12 19:06:48 America/Los_Angeles"); got ${JSON.stringify(v)}, which the server would read in the session's time zone.`);
    }
  } else if (v instanceof Date && kind.split("|").every((part) => part === "timestamp" || part === "date")) {
    throw new ParameterError(`Parameter "${name}" is a ${kind === "date" ? "date" : "timestamp"} without a time zone, so it takes the wall-clock text ("2026-10-12 19:06:48.282", "2026-10-12"), not a Date: drivers write a Date's time in different zones (the app's, the server's or UTC).`);
  }
}

// A binary parameter takes a Uint8Array (a Buffer is one), as `.run()` returns binary data and
// as a structured parameter's binary member does (src/rows.ts). Anything else would reach the
// driver as it is, and a string means something different on each database: Postgres reads it
// as bytea's hex or escape text, MySQL stores the bytes of its UTF-8, and SQLite stores it as
// TEXT, which comes back a string. So base64 text was stored as its own letters.
function checkBinary(type: EngineType | undefined, name: string, v: unknown, dialect: Dialect): void {
  if (type === undefined || v == null) return;
  if (type.kind === "array" && columnKind(type, dialect).endsWith("[]")) {
    if (Array.isArray(v)) for (const e of v) checkBinary(Array.isArray(e) ? type : type.element.type, name, e, dialect);
    return;
  }
  if (columnKind(type, dialect) !== "bytes" || v instanceof Uint8Array) return;
  throw new ParameterError(`Parameter "${name}" is binary data, so it takes a Uint8Array (a Buffer is one), as \`.run()\` returns one; got ${describe(v)}. ${v instanceof ArrayBuffer ? "Wrap an ArrayBuffer in one: new Uint8Array(buffer)." : "Decode base64 or hex text into one first."}`);
}

/** A vector's floats as the text a VECTOR's `to_vector` and pgvector's input read (src/values.ts), which hold no NaN or infinity, so those are refused. */
export function vectorParameter(v: readonly unknown[], name: string, what: string): string {
  v.forEach((e, i) => {
    if (typeof e !== "number" || !Number.isFinite(e)) {
      throw new ParameterError(`Parameter "${name}" is ${what}, an array of finite numbers, and the element at [${i}] is ${typeof e === "string" ? JSON.stringify(e) : typeof e === "number" ? e : typeof e}.`);
    }
  });
  return vectorText(v as readonly number[]);
}

/** Binary data as Postgres's hex form, `\x0102ff`: Neon's neon() before 0.10 sends a Buffer as JSON, and 0.10 and later send one as this form themselves. */
export function asByteaHex(v: unknown) {
  if (!(v instanceof Uint8Array)) return v;
  let hex = "\\x";
  for (const byte of v) hex += (byte < 16 ? "0" : "") + byte.toString(16);
  return hex;
}

/**
 * A Postgres array literal, written as pg writes one (`arrayString` in pg/lib/utils.js): a nested
 * array in braces, null and undefined as NULL, binary data as `\\x` and its hex, and every other
 * element quoted, with `"` and `\` escaped: a Date as its local time and offset, an object as
 * its JSON, anything else as its string. The elements are separated by `delim`, the element type's
 * delimiter: the server reads `{"a","b"}` as one element of a `geometry[]`, whose delimiter is `:`.
 */
export function arrayLiteral(array: readonly unknown[], delim = ","): string {
  let out = "{";
  for (let i = 0; i < array.length; i++) {
    const v = array[i];
    if (i) out += delim;
    if (v == null) out += "NULL";
    else if (Array.isArray(v)) out += arrayLiteral(v, delim);
    else if (ArrayBuffer.isView(v)) out += "\\" + asByteaHex(new Uint8Array(v.buffer, v.byteOffset, v.byteLength));
    else out += `"${(v instanceof Date ? dateText(v) : typeof v === "object" ? JSON.stringify(v) : String(v)).replace(/[\\"]/g, "\\$&")}"`;
  }
  return out + "}";
}

/** pg's `dateToString`: the local date and time to the millisecond, then the offset; BC for a year before 1. */
function dateText(d: Date): string {
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  const year = d.getFullYear();
  const offset = -d.getTimezoneOffset();
  const abs = Math.abs(offset);
  return `${pad(year < 1 ? 1 - year : year, 4)}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}${offset < 0 ? "-" : "+"}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}${year < 1 ? " BC" : ""}`;
}

/**
 * The statement as Reylo sends it to the database, the parameter each value binds, and each
 * placeholder's place in it: the engine's compiled text where a build step wrote one into the
 * module. A query defined by hand without it is taken as written: its `sql` is the statement the
 * database receives, in the database's own placeholders (`$1` on Postgres, `?` on MySQL and
 * SQLite), and its `params` the binding order, one name per value. Nothing in the run time reads
 * a placeholder off the text: only the engine's lexer knows every string and comment of each dialect.
 */
export function compile(query: QueryModule): { text: string; order: readonly string[]; marks: Marks | undefined } {
  if (query.text && query.order) return { text: query.text, order: query.order, marks: query.marks };
  return { text: query.sql, order: query.params, marks: query.marks };
}

function unsupported(c: unknown, { dialect, runtime }: QueryModule): TypeError {
  let seen: string;
  if (c === null || (typeof c !== "object" && typeof c !== "function")) seen = String(c);
  else {
    const names = new Set<string>();
    for (let o: object | null = c; o && o !== Object.prototype && o !== Function.prototype; o = Object.getPrototypeOf(o)) {
      for (const key of Object.getOwnPropertyNames(o)) {
        const d = Object.getOwnPropertyDescriptor(o, key);
        if (key !== "constructor" && typeof d?.value === "function") names.add(key);
      }
    }
    seen = names.size ? `an object with ${[...names].slice(0, 12).join(", ")}${names.size > 12 ? ", ..." : ""}` : "an object with no methods";
  }
  // node:sqlite's tag store keeps its database on db. Kysely and Prisma decode every value of a
  // raw query themselves, by their own rules or the driver's parsers, before it returns.
  // Neither hands out a transaction's connection.
  const o = c as { db?: { prepare?: unknown }; selectFrom?: unknown; executeQuery?: unknown; isTransaction?: unknown; $queryRawUnsafe?: unknown; $transaction?: unknown } | null;
  const inTx = " and does not hand out its transaction's connection: take the transaction from the driver's client instead.";
  const hint =
    typeof o?.db?.prepare === "function"
      ? " A tag store is not a client: pass its database, store.db."
      : typeof o?.selectFrom === "function" && typeof o.executeQuery === "function"
        ? ` Kysely decodes the values of its queries itself${o.isTransaction === true ? inTx : ": pass the pool or database its dialect was made with."}`
        : typeof o?.$queryRawUnsafe === "function"
          ? ` Prisma decodes the values of its queries itself${typeof o.$transaction !== "function" ? inTx : ": pass a client of your database's driver, such as the pool its driver adapter was made with."}`
          : dialect === "postgres" && (typeof c === "function" || typeof (o as { query?: unknown } | null)?.query === "function" || typeof (o as { unsafe?: unknown } | null)?.unsafe === "function")
              ? " It is none of those drivers' clients. A wrapper around one hands back the values its driver already converted, a Date for a timestamp and a string for a bigint, which reylo cannot read back: pass the driver's own pool, client or transaction, or keep the wrapper one of them, as a subclass or a proxy."
              : "";
  return new TypeError(`reylo cannot run a ${dialect} statement on ${seen}. It takes a client with ${runtime.clients}.${hint}`);
}
