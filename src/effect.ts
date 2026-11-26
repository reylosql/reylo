// The run time inside Effect SQL (`effect/sql`, Effect 4): a query as an Effect that runs on the
// `SqlClient` its context provides, on the open transaction's connection inside `withTransaction`,
// with every value decoded by Reylo as `.run()` decodes it. It is the one module of the package
// that imports `effect`, an optional peer dependency, so an app without Effect loads none of it.
//
// Effect SQL calls each driver itself, with its own settings, so Reylo cannot hand the driver its
// per-query decoding as it does on the driver's own client (src/decode.ts and its siblings). Each
// Effect client is reached where it keeps what Reylo needs:
//
// - MySQL (`@effect/sql-mysql2`): the connection keeps mysql2's PoolConnection on `conn`, which
//   `.run()` takes as it is, so the statement runs with Reylo's own per-query options. It holds the
//   connection's `permit` as Effect's own statements do, and an interrupted one is stopped with
//   `KILL QUERY` before the connection is given back (`onMysql`).
// - Postgres (`@effect/sql-pg`): Effect's own client of the wire protocol reads every result in
//   the binary format and decodes it with its own codecs; its `queryTextValues` reads one statement's
//   columns in the text format instead, each value the server's text, which Reylo decodes as it
//   does pg's (src/decode.ts).
// - SQLite (`@effect/sql-sqlite-node`, `-sqlite-bun`, `-sqlite-do`, `-d1`, `-libsql`): each
//   driver turns a storage class into a JavaScript value in native code, as on its own client, so
//   Reylo normalizes the rows Effect's statement returns (src/decode-sqlite.ts), with Effect's
//   `SafeIntegers` on for the statement on the two clients that read it, so that an INTEGER past
//   2^53 comes back exact, and with no `CurrentTransformer`, which would rewrite the statement.
//
// Each statement checks that its Effect was not interrupted before it starts, so that a statement
// Reylo runs after another (a check, a catalog query) is never sent once the caller has gone.

import { Effect, Option, type Context, type Scope, type Semaphore } from "effect";
import { SqlClient, SqlError, Statement } from "effect/sql";
import { EFFECT_SQL, type Dialect, type EffectSql } from "./execute.ts";
import type { Cardinality, Materialize, ParamsArg, Query } from "./index.ts";

/** An Effect SQL connection, as each client's own class implements one. */
type Connection = { readonly executeUnprepared: (sql: string, params: ReadonlyArray<unknown>, transformRows: undefined) => Effect.Effect<unknown, SqlError.SqlError> };

/** @effect/sql-mysql2's connection: mysql2's PoolConnection on `conn`, and the permit every statement on it holds. */
type MysqlConnection = Connection & { readonly conn?: { promise?: unknown; threadId?: number; destroy?: () => void }; readonly permit?: Semaphore.Semaphore };

/** The few members of a mysql2 connection the kill below calls. */
type KillConnection = { on(event: "error", listener: () => void): unknown; query(sql: string, values: unknown[], callback: () => void): unknown; destroy(): void };

/** Each Effect SQL client's `driver`, by the dialect whose statements it runs. */
const DRIVERS: { readonly [D in Dialect]: readonly string[] } = {
  postgres: ["pg"],
  mysql: ["mysql2"],
  sqlite: ["sqlite-node", "sqlite-bun", "sqlite-do", "d1", "libsql"],
  clickhouse: [],
};

/**
 * Runs the query on the `SqlClient` in the Effect's context: on the transaction's connection
 * inside `sql.withTransaction`, else on a connection of the client's. It fails with Effect's
 * `SqlError` where the database raised an error; a value Reylo refuses, as `.run()` refuses it,
 * is a defect, as it rejects `.try()`.
 */
export function effect<Params, Row, C extends Cardinality, D extends Dialect>(query: Query<Params, Row, C, D>, ...[params]: ParamsArg<Params>): Effect.Effect<Materialize<Row, C>, SqlError.SqlError, SqlClient.SqlClient> {
  const args = [params] as ParamsArg<Params>;
  return Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient;
    const drivers = DRIVERS[query.dialect];
    if (!drivers.includes(sql.driver)) {
      return yield* Effect.die(new TypeError(`reylo cannot run a ${query.dialect} statement on Effect's ${sql.driver} client: it runs ${query.dialect} statements on ${drivers.length ? `Effect's ${drivers.join(", ")}` : "no Effect SQL client yet"}.`));
    }
    if (query.dialect === "mysql") {
      return yield* onConnection(sql, (conn) => {
        const mysql2 = conn.conn;
        if (mysql2 == null || typeof mysql2.promise !== "function" || conn.permit === undefined) return Effect.die(new TypeError(`reylo cannot reach mysql2's connection on this Effect MySQL client: it runs on the PoolConnection @effect/sql-mysql2 keeps on its connection's conn, under its permit.`));
        return onMysql(sql, conn as Required<MysqlConnection>, () => query.run(mysql2 as never, ...args));
      });
    }
    // A failed statement rejects with the `SqlError` Effect's client raised, classified by Effect.
    const context = yield* Effect.context<never>();
    const failure = (error: unknown) => (SqlError.isSqlError(error) ? error : undefined);
    if (query.dialect === "postgres") {
      const pg = sql as SqlClient.SqlClient & Partial<PgText>;
      if (typeof pg.queryTextValues !== "function") return yield* Effect.die(new TypeError(`reylo cannot read this Effect Postgres client's results: it decodes each value itself from the server's text, which @effect/sql-pg's queryTextValues() hands over, and this version of @effect/sql-pg has none. Update @effect/sql-pg.`));
      return yield* settle((signal) => query.run(postgresClient(pg as PgText & object, context, signal) as never, ...args), failure);
    }
    return yield* settle((signal) => query.run(sqliteClient(sql, context, signal) as never, ...args), failure);
  });
}

/**
 * The promise's value, its rejection as the Effect's failure where `failure` reads one, and any
 * other rejection (a value Reylo refuses, a row count the statement cannot return) as a defect, as
 * `.try()` gives back the database's errors alone.
 */
function settle<A>(run: (signal: AbortSignal) => Promise<A>, failure: (error: unknown) => SqlError.SqlError | undefined): Effect.Effect<A, SqlError.SqlError> {
  return Effect.flatten(
    Effect.promise((signal) =>
      run(signal).then(
        (value) => Effect.succeed(value),
        (error) => {
          const failed = failure(error);
          return failed ? Effect.fail(failed) : Effect.die(error);
        },
      ),
    ),
  );
}

/** The transaction's connection inside `withTransaction`, else one of the client's own for the statement. */
function onConnection<A>(sql: SqlClient.SqlClient, f: (conn: MysqlConnection) => Effect.Effect<A, SqlError.SqlError>): Effect.Effect<A, SqlError.SqlError> {
  return Effect.flatMap(Effect.serviceOption(sql.transactionService), (transaction) =>
    Option.isSome(transaction)
      ? Effect.flatMap(SqlClient.activeTransactionConnection(transaction.value as unknown as readonly [MysqlConnection, number]), f)
      : Effect.scoped(Effect.flatMap(sql.reserve as unknown as Effect.Effect<MysqlConnection, SqlError.SqlError, Scope.Scope>, f)),
  );
}

/** How long an interrupted MySQL statement waits for `KILL QUERY` and its own end before its connection is destroyed, as @effect/sql-mysql2's waits. */
const MYSQL_CANCEL_MS = 5000;

/**
 * Reylo's statement on the mysql2 connection Effect lent it, as @effect/sql-mysql2 runs its own: it
 * holds the connection's permit from before its first command until it has settled, so a
 * transaction's statements run one at a time and a `KILL QUERY` reaches only the one interrupted.
 * Interrupted, it sends `KILL QUERY` for the connection's thread over a connection of its own,
 * outside the pool, which may have no free slot, then waits for the statement to end before the
 * connection is given back, or destroys the connection after `MYSQL_CANCEL_MS`. A database or
 * driver error fails it with `SqlError`.
 */
function onMysql<A>(sql: SqlClient.SqlClient, conn: Required<MysqlConnection>, run: () => Promise<A>): Effect.Effect<A, SqlError.SqlError> {
  const statement = Effect.callback<A, SqlError.SqlError>((resume) => {
    let settled = false;
    let cancelling = false;
    let onSettled: (() => void) | undefined;
    // True where the caller still takes the outcome, false where a cancellation is waiting for it.
    const settle = () => {
      if (settled) return false;
      settled = true;
      if (!cancelling) return true;
      onSettled?.();
      return false;
    };
    run().then(
      (value) => settle() && resume(Effect.succeed(value)),
      (error) => settle() && resume(isMysqlError(error) ? Effect.fail(mysqlError(error)) : Effect.die(error)),
    );
    return Effect.suspend(() => {
      cancelling = true;
      if (settled) return Effect.void;
      return Effect.callback<void>((done) => {
        let finished = false;
        let killed = false;
        let finish = () => {
          if (finished) return;
          finished = true;
          clearTimeout(timer);
          done(Effect.void);
        };
        const timer = setTimeout(() => {
          conn.conn.destroy?.();
          finish();
        }, MYSQL_CANCEL_MS);
        onSettled = () => killed && finish();
        const onKilled = () => {
          killed = true;
          if (settled) finish();
        };
        let kill: KillConnection | undefined;
        const end = finish;
        // A kill that fails, or a connection that cannot be made, is ignored.
        finish = () => {
          kill?.destroy();
          end();
        };
        try {
          kill = killConnection(conn.conn);
          kill.on("error", () => {});
          kill.query("KILL QUERY ?", [conn.conn.threadId], onKilled);
        } catch {
          onKilled();
        }
      });
    });
  });
  return conn.permit.withPermit(statement);
}

/**
 * A connection of its own to the server a pool connection of mysql2's reaches, as the pool would
 * make one: mysql2's `Connection`, which `PoolConnection` extends, on a copy of the pool's config.
 */
function killConnection(pooled: unknown): KillConnection {
  const c = pooled as { constructor: Function; config: object; _pool?: { _createConnectionConfig?: () => object } };
  const Connection = Object.getPrototypeOf(c.constructor) as new (options: { config: object }) => KillConnection;
  const config = c._pool?._createConnectionConfig?.() ?? Object.create(Object.getPrototypeOf(c.config), Object.getOwnPropertyDescriptors(c.config));
  return new Connection({ config });
}

// mysql2's error numbers as @effect/sql-mysql2 classifies them, so that an error reads the same
// whichever of the two ran the statement. An error with no number, as a lost connection's, is
// UnknownError there too.
const MYSQL_CONNECTION = new Set([1040, 1042, 1043, 1129, 1130, 1203]);
const MYSQL_AUTHORIZATION = new Set([1044, 1142, 1143, 1227]);
const MYSQL_SYNTAX = new Set([1054, 1064, 1146]);
const MYSQL_CONSTRAINT = new Set([1022, 1048, 1169, 1216, 1217, 1451, 1452, 1557]);

/** An error mysql2 raised, the database's or its own (a lost connection, a reset socket, a closed connection), which has a code, a number or `fatal`; Reylo's own errors have none. */
function isMysqlError(error: unknown): error is Error & { errno?: unknown; code?: unknown; sqlMessage?: unknown } {
  const e = error as { code?: unknown; errno?: unknown; fatal?: unknown };
  return error instanceof Error && !(error instanceof TypeError) && (typeof e.code === "string" || typeof e.errno === "number" || e.fatal === true);
}

/** The key a duplicate entry names, as @effect/sql-mysql2 reads it from mysql2's message. */
function duplicateKey(error: { sqlMessage?: unknown; message?: unknown }): string {
  for (const text of [error.sqlMessage, error.message]) {
    const m = typeof text === "string" ? /\bfor key\s+(?:'([^']*)'|`([^`]*)`|([^\s'`]+))/i.exec(text) : null;
    const key = (m?.[1] ?? m?.[2] ?? m?.[3])?.trim();
    if (key) return key;
  }
  return "unknown";
}

/** An error mysql2 raised as Effect's `SqlError`, classified as @effect/sql-mysql2 classifies it, its cause mysql2's error. */
function mysqlError(error: Error & { errno?: unknown; sqlMessage?: unknown }): SqlError.SqlError {
  const errno = error.errno;
  const props = { cause: error, message: "Failed to execute statement", operation: "execute" };
  const reason =
    typeof errno !== "number"
      ? new SqlError.UnknownError(props)
      : MYSQL_CONNECTION.has(errno)
        ? new SqlError.ConnectionError(props)
        : errno === 1045
          ? new SqlError.AuthenticationError(props)
          : MYSQL_AUTHORIZATION.has(errno)
            ? new SqlError.AuthorizationError(props)
            : MYSQL_SYNTAX.has(errno)
              ? new SqlError.SqlSyntaxError(props)
              : errno === 1062
                ? new SqlError.UniqueViolation({ ...props, constraint: duplicateKey(error) })
                : MYSQL_CONSTRAINT.has(errno)
                  ? new SqlError.ConstraintError(props)
                  : errno === 1213
                    ? new SqlError.DeadlockError(props)
                    : errno === 1205
                      ? new SqlError.LockTimeoutError(props)
                      : errno === 3024
                        ? new SqlError.StatementTimeoutError(props)
                        : new SqlError.UnknownError(props);
  return new SqlError.SqlError({ reason });
}

/**
 * The client `.run()` takes for a SQLite statement on Effect SQL (src/decode-sqlite.ts): each
 * statement through the client's own `unsafe`, in the Effect's context, so that inside
 * `withTransaction` it runs on the transaction's connection. A failed statement rejects with
 * Effect's `SqlError`, which `.try()` reads as the database's error it holds.
 */
function sqliteClient(sql: SqlClient.SqlClient, context: Context.Context<never>, signal: AbortSignal): EffectSql {
  const exact = sql.driver === "sqlite-node" || sql.driver === "sqlite-bun";
  const runPromise = Effect.runPromiseWith(context);
  const read = <A>(statement: Effect.Effect<A, SqlError.SqlError>) => {
    if (signal.aborted) return Promise.reject(signal.reason);
    const own = Effect.provideService(statement, Statement.CurrentTransformer, undefined);
    return runPromise(exact ? Effect.provideService(own, SqlClient.SafeIntegers, true) : own, { signal });
  };
  return {
    [EFFECT_SQL]: {
      dialect: "sqlite",
      driver: sql.driver,
      exact,
      values: (text, values) => read(sql.unsafe(text, values).values) as Promise<unknown[][]>,
      objects: (text, values) => read(sql.unsafe(text, values).raw) as Promise<unknown[]>,
    },
  };
}

/** `queryTextValues` of @effect/sql-pg's client: one statement, every column in the text format, its rows by position. */
type PgText = {
  readonly queryTextValues: (sql: string, params?: ReadonlyArray<unknown>) => Effect.Effect<{ readonly fields: ReadonlyArray<{ readonly name: string; readonly dataTypeId: number }>; readonly values: ReadonlyArray<ReadonlyArray<string | null>> }, SqlError.SqlError>;
};

/**
 * The client `.run()` takes for a Postgres statement on Effect SQL (src/decode.ts): each statement
 * through the client's `queryTextValues`, in the Effect's context, so that inside `withTransaction` it
 * runs on the transaction's connection. What Reylo learns of the database's own types it keeps
 * under the client.
 */
function postgresClient(pg: PgText & object, context: Context.Context<never>, signal: AbortSignal): EffectSql {
  const runPromise = Effect.runPromiseWith(context);
  return { [EFFECT_SQL]: { dialect: "postgres", key: pg, text: (text, values) => (signal.aborted ? Promise.reject(signal.reason) : runPromise(pg.queryTextValues(text, values), { signal })) } };
}
