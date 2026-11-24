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
//   `.run()` takes as it is, so the statement runs with Reylo's own per-query options.
// - Postgres (`@effect/sql-pg`): Effect's own client of the wire protocol reads every result in
//   the binary format and decodes it with its own codecs; its `queryText` reads one statement's
//   columns in the text format instead, each value the server's text, which Reylo decodes as it
//   does pg's (src/decode.ts).
// - SQLite (`@effect/sql-sqlite-node`, `-sqlite-bun`, `-sqlite-wasm`, `-sqlite-do`, `-d1`,
//   `-libsql`): each driver turns a storage class into a JavaScript value in native code, as on
//   its own client, so Reylo normalizes the rows Effect's statement returns (src/decode-sqlite.ts),
//   with Effect's `SafeIntegers` on for the statement on the two clients that read it, so that an
//   INTEGER past 2^53 comes back exact.

import { Effect, Option, type Context, type Scope } from "effect";
import { SqlClient, SqlError } from "effect/sql";
import { EFFECT_SQL, type Dialect, type EffectSql } from "./execute.ts";
import type { Cardinality, Materialize, ParamsArg, Query } from "./index.ts";
import type { QueryError } from "./query-error.ts";

/** An Effect SQL connection, as each client's own class implements one; mysql2's keeps its PoolConnection on `conn`. */
type Connection = { readonly conn?: { promise?: unknown } };

/** Each Effect SQL client's `driver`, by the dialect whose statements it runs. */
const DRIVERS: { readonly [D in Dialect]: readonly string[] } = {
  postgres: ["pg"],
  mysql: ["mysql2"],
  sqlite: ["sqlite-node", "sqlite-bun", "sqlite-wasm", "sqlite-do", "d1", "libsql"],
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
        if (mysql2 == null || typeof mysql2.promise !== "function") return Effect.die(new TypeError(`reylo cannot reach mysql2's connection on this Effect MySQL client: it runs on the PoolConnection @effect/sql-mysql2 keeps on its connection's conn.`));
        return Effect.flatMap(settle(() => query.try(mysql2 as never, ...args), () => undefined), (result) => (result.error === undefined ? Effect.succeed(result.value as Materialize<Row, C>) : Effect.fail(mysqlError(result.error as QueryError<"mysql">))));
      });
    }
    // A failed statement rejects with the `SqlError` Effect's client raised, classified by Effect.
    const context = yield* Effect.context<never>();
    const failure = (error: unknown) => (SqlError.isSqlError(error) ? error : undefined);
    if (query.dialect === "postgres") {
      const pg = sql as SqlClient.SqlClient & Partial<PgText>;
      if (typeof pg.queryText !== "function") return yield* Effect.die(new TypeError(`reylo cannot read this Effect Postgres client's results: it decodes each value itself from the server's text, which @effect/sql-pg's queryText() hands over, and this version of @effect/sql-pg has none. Update @effect/sql-pg.`));
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
function onConnection<A>(sql: SqlClient.SqlClient, f: (conn: Connection) => Effect.Effect<A, SqlError.SqlError>): Effect.Effect<A, SqlError.SqlError> {
  return Effect.flatMap(Effect.serviceOption(sql.transactionService), (transaction) =>
    Option.isSome(transaction)
      ? Effect.flatMap(SqlClient.activeTransactionConnection(transaction.value as readonly [Connection, number]), f)
      : Effect.scoped(Effect.flatMap(sql.reserve as Effect.Effect<Connection, SqlError.SqlError, Scope.Scope>, f)),
  );
}

// mysql2's error numbers as @effect/sql-mysql2 classifies them, so that an error reads the same
// whichever of the two ran the statement.
const MYSQL_CONNECTION = new Set([1040, 1042, 1043, 1129, 1130, 1203]);
const MYSQL_AUTHORIZATION = new Set([1044, 1142, 1143, 1227]);
const MYSQL_SYNTAX = new Set([1054, 1064, 1146]);
const MYSQL_CONSTRAINT = new Set([1022, 1048, 1169, 1216, 1217, 1451, 1452, 1557]);

/** A MySQL error, as Reylo read it from mysql2's, as Effect's `SqlError`, its cause Reylo's reading. */
function mysqlError(error: QueryError<"mysql">): SqlError.SqlError {
  const errno = (error.cause as { errno?: unknown } | undefined)?.errno;
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
                ? new SqlError.UniqueViolation({ ...props, constraint: error.key ?? "unknown" })
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
  const read = <A>(statement: Effect.Effect<A, SqlError.SqlError>) => runPromise(exact ? Effect.provideService(statement, SqlClient.SafeIntegers, true) : statement, { signal });
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

/** `queryText` of @effect/sql-pg's client: one statement, every column in the text format, its rows by position. */
type PgText = {
  readonly queryText: (sql: string, params?: ReadonlyArray<unknown>) => Effect.Effect<{ readonly fields: ReadonlyArray<{ readonly name: string; readonly dataTypeId: number }>; readonly rows: ReadonlyArray<ReadonlyArray<string | null>> }, SqlError.SqlError>;
};

/**
 * The client `.run()` takes for a Postgres statement on Effect SQL (src/decode.ts): each statement
 * through the client's `queryText`, in the Effect's context, so that inside `withTransaction` it
 * runs on the transaction's connection. What Reylo learns of the database's own types it keeps
 * under the client.
 */
function postgresClient(pg: PgText & object, context: Context.Context<never>, signal: AbortSignal): EffectSql {
  const runPromise = Effect.runPromiseWith(context);
  return { [EFFECT_SQL]: { dialect: "postgres", key: pg, text: (text, values) => runPromise(pg.queryText(text, values), { signal }) } };
}
