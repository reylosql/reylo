// `migrate` from reylo's root: `reylo migrate deploy`, with three hosts. On a server, `await migrate()`
// runs the binary's deploy, as the command line does (src/migrate-deploy.ts, through the build-time
// half). Where the app holds the only connection to its database, the deploy's last step runs here,
// on that connection: on a Cloudflare Durable Object, whose SQLite database nothing outside the
// object can reach, `migrate(ctx.storage)`; on an app's own SQLite connection, as Turso's engine
// (`@tursodatabase/database`) holds its file, `migrate(db)`. Both run each migration the database
// hasn't run yet, in the order the deploy runs them, recorded in the history table `reylo migrate`
// keeps on SQLite (`history_ddl` in engine/migrate/src/commands.rs), so that both read the same rows.
// Before running any, they hold the history against the migrations as a deploy does with no shadow
// database, and refuse a history the migrations don't continue. One series of steps, `steps`, says
// what runs; the Durable Object drives it synchronously, inside `transactionSync`, and a connection
// drives it through its own `begin` and `commit`, awaiting each answer.
import { buildTime, type MigrateOptions } from "./build-time.ts";
import { embedded } from "./embedded-migrations.ts";
import { sameMigration } from "./migration-text.ts";

/** A Durable Object's storage, `ctx.storage`, as far as `migrate` uses it. */
export type MigrationStorage = {
  readonly sql: { exec(query: string, ...bindings: any[]): { toArray(): unknown[] } };
  transactionSync<T>(closure: () => T): T;
};

/**
 * An app's own connection to a SQLite database, as far as `migrate` uses it: `exec` runs one or more
 * statements, and `prepare(sql)` gives a statement whose `all` and `run` take its values. The
 * Database of Turso's `@tursodatabase/database` answers through promises; its compat Database,
 * better-sqlite3, `node:sqlite` and `bun:sqlite` answer at once.
 */
export type MigrationConnection = {
  exec(sql: string): unknown;
  prepare(sql: string): unknown;
};

/** A migration as the bundle carries it: its file's name and its SQL as written. */
export type Migration = { readonly name: string; readonly sql: string };

const HISTORY = "__reylo_migrations";

/** What runs: a script, a query whose rows come back, one statement with its values, or a transaction's ends. */
type Step = { exec: string } | { all: string } | { run: string; values: unknown[] } | "begin" | "commit";

/** The database the error messages name, and where its migrations came from. */
type Words = { database: string; source: string; sourceOrder: string; restore: string };

const OBJECT: Words = { database: "this Durable Object", source: "this build", sourceOrder: "this build's order", restore: "deploy a build whose migrations include it" };
const CONNECTION: Words = { database: "this database", source: "the project", sourceOrder: "the lock's order", restore: "put it back in the project's migrations" };

/**
 * The steps of a deploy's last step on SQLite: the history table made, the history held against
 * `list`, then each migration the database hasn't run, in its own transaction with its row in the
 * history table. Each step's answer comes back as the value of its `yield`: a query's rows.
 */
function* steps(list: readonly Migration[], words: Words): Generator<Step, string[], unknown[] | undefined> {
  yield { exec: `create table if not exists ${HISTORY} (name text primary key, sql text not null, applied_at text, fingerprint text, catalog text)` };
  // In the order they ran, as reylo migrate reads its history (`read_history`).
  const ran = ((yield { all: `select name, sql from ${HISTORY} order by applied_at is null, applied_at, rowid, case when name = 'init.sql' then '' else name end` }) ?? []) as { name: string; sql: string }[];
  const listed = new Map(list.map((m) => [m.name, m]));
  ran.forEach((row, i) => {
    const m = listed.get(row.name);
    if (!m) throw new Error(`reylo: ${words.database} ran the migration ${row.name}, which ${words.source} doesn't have: ${words.restore}`);
    // As the lock compares them: comments, spacing and line endings aside (src/migration-text.ts).
    if (!sameMigration(m.sql, row.sql)) throw new Error(`reylo: the migration ${row.name} isn't the text ${words.database} ran: put the change in a new migration instead of editing one a database may have run`);
    if (list[i]?.name !== row.name) throw new Error(`reylo: ${words.database} ran ${row.name} where ${words.sourceOrder} has ${list[i]?.name ?? "nothing"} first: migrations run in the lock's order, so add a new one after those that ran`);
  });
  const done = new Set(ran.map((r) => r.name));
  const applied: string[] = [];
  for (const m of list) {
    if (done.has(m.name)) continue;
    try {
      yield "begin";
      yield { exec: m.sql };
      // As reylo migrate checks a SQLite migration's foreign keys before it commits (`run` in
      // engine/migrate/src/commands.rs): a deferred one it broke fails here, rolling back the
      // migration alone, rather than at the object's commit, which resets the object. Turso's
      // engine answers this pragma with no rows, and fails the commit instead.
      const broken = ((yield { all: "pragma foreign_key_check" }) ?? []).length;
      if (broken) throw new Error(`${broken} rows break a foreign key after this migration`);
      // The check read every key and found each whole, so what SQLite still counts as deferred
      // violations is none: a table's drop counts its children's rows, and making it again does
      // not uncount them. Left counted, they fail the object's commit, which resets the object,
      // and with the migration's row gone too, every start runs it and resets again.
      yield { exec: "pragma defer_foreign_keys = off" };
      yield { run: `insert into ${HISTORY} (name, sql, applied_at) values (?, ?, strftime('%Y-%m-%d %H:%M:%f', 'now'))`, values: [m.name, m.sql] };
      yield "commit";
    } catch (e) {
      throw new Error(`reylo: the migration ${m.name} failed on ${words.database}, and rolled back: ${(e as Error).message}`, { cause: e });
    }
    applied.push(m.name);
  }
  return applied;
}

/**
 * Runs, on a SQLite-backed Durable Object's storage, each of `list` the database hasn't run, in
 * order, each in a transaction together with its row in the history table, and returns the names it
 * ran. A migration that fails rolls back with its row, and the error names it.
 *
 * Before it runs any, it refuses a history the bundle doesn't continue, naming the migration: one
 * the object ran that the bundle lacks, one whose text in the bundle isn't the text it ran, and
 * migrations it ran in another order than the bundle's.
 */
export function runMigrations(storage: MigrationStorage, list: readonly Migration[]): string[] {
  const { sql } = storage;
  const run = steps(list, OBJECT);
  let next = run.next();
  // A step's answer, and the next step: a failure goes back into the steps, which name the migration.
  const answer = (step: Step): IteratorResult<Step, string[]> => {
    let rows: unknown[] | undefined;
    try {
      rows = typeof step === "string" ? undefined : "run" in step ? (sql.exec(step.run, ...step.values), undefined) : sql.exec("exec" in step ? step.exec : step.all).toArray();
    } catch (e) {
      return run.throw(e);
    }
    return run.next(rows);
  };
  while (!next.done) {
    if (next.value !== "begin") {
      next = answer(next.value);
      continue;
    }
    // A Durable Object refuses `begin`: a transaction is a closure, which rolls back where it throws.
    next = storage.transactionSync(() => {
      let inside = run.next();
      while (!inside.done && inside.value !== "commit") inside = answer(inside.value);
      return inside;
    });
    if (!next.done) next = run.next();
  }
  return next.value;
}

/** Runs `list` on an app's own SQLite connection, as `runMigrations` does on a Durable Object, each migration between its own `begin` and `commit`. */
export async function runMigrationsOn(db: MigrationConnection, list: readonly Migration[]): Promise<string[]> {
  const run = steps(list, CONNECTION);
  const statement = async (sql: string) => (await db.prepare(sql)) as { all(...values: unknown[]): unknown; run(...values: unknown[]): unknown };
  let open = false;
  let next = run.next();
  try {
    while (!next.done) {
      const step = next.value;
      let rows: unknown[] | undefined;
      try {
        if (step === "begin") {
          await db.exec("begin immediate");
          open = true;
        } else if (step === "commit") {
          await db.exec("commit");
          open = false;
        } else if ("exec" in step) await db.exec(step.exec);
        else if ("all" in step) rows = (await (await statement(step.all)).all()) as unknown[];
        else await (await statement(step.run)).run(...step.values);
      } catch (e) {
        next = run.throw(e);
        continue;
      }
      next = run.next(rows);
    }
  } catch (e) {
    // A commit that failed, as on a deferred foreign key, leaves its transaction open: it rolls back
    // here. A rollback that fails had none to roll back, as after an error SQLite rolled back itself.
    if (open) await Promise.resolve().then(() => db.exec("rollback")).catch(() => {});
    throw e;
  }
  return next.value;
}

const isStorage = (v: unknown): v is MigrationStorage => typeof v === "object" && v !== null && typeof (v as MigrationStorage).transactionSync === "function" && typeof (v as MigrationStorage).sql?.exec === "function";

const isConnection = (v: unknown): v is MigrationConnection => typeof v === "object" && v !== null && typeof (v as MigrationConnection).exec === "function" && typeof (v as MigrationConnection).prepare === "function";

/** The project's migrations in the order a deploy runs them: those Reylo's bundler plugin put in the bundle, else those in the project's directory. */
async function projectMigrations(): Promise<readonly Migration[]> {
  if (embedded !== undefined) {
    if ("error" in embedded) throw new Error(`reylo: ${embedded.error}`);
    return embedded as readonly Migration[];
  }
  const { list, error } = await buildTime().migrations();
  if (error !== undefined) throw new Error(`reylo: ${error}`);
  return list ?? [];
}

/**
 * Runs the project's migrations on a SQLite-backed Durable Object's storage, `ctx.storage`: each the
 * object hasn't run yet, in the order `reylo migrate deploy` runs them, each in a transaction with its
 * row in `__reylo_migrations`. Returns the names it ran. Call it in the object's constructor.
 *
 * Reylo's Vite or esbuild plugin puts the locked migrations in the bundle. Before it runs any, it
 * throws, running nothing, where the object ran a migration the bundle lacks, has edited or orders
 * differently.
 */
export function migrate(storage: MigrationStorage): string[];
/**
 * Runs the project's migrations on the app's own connection to a SQLite database, such as the
 * Database of Turso's `@tursodatabase/database`: each it hasn't run yet, in the order
 * `reylo migrate deploy` runs them, each in a transaction with its row in `__reylo_migrations`.
 * Resolves to the names it ran. Call it as the app starts, before it serves requests.
 *
 * The migrations are those in the bundle, where Reylo's Vite or esbuild plugin built it, else the
 * project's locked migrations. Before it runs any, it rejects, running nothing, where the database
 * ran a migration the project lacks, has edited or orders differently.
 */
export function migrate(db: MigrationConnection): Promise<string[]>;
/**
 * Runs `reylo migrate deploy` on the project's database, or the one `url` names, as the command line
 * does, and resolves to the names of the migrations it ran. Rejects with the deploy's problems where
 * it refuses, as on a draft or a database changed by hand. It runs on Node and Bun, from the
 * project's directory.
 */
export function migrate(options?: MigrateOptions): Promise<string[]>;
export function migrate(arg?: MigrationStorage | MigrationConnection | MigrateOptions): string[] | Promise<string[]> {
  if (isStorage(arg)) {
    if (embedded === undefined) throw new Error("reylo: this bundle holds no migrations for migrate: build it with reylo/vite or reylo/esbuild, in a project whose migrations are in reylo/migrations");
    if ("error" in embedded) throw new Error(`reylo: ${embedded.error}`);
    return runMigrations(arg, embedded as readonly Migration[]);
  }
  // A promise that rejects, never a throw, where the build-time half can't be loaded.
  if (isConnection(arg)) return Promise.resolve().then(projectMigrations).then((list) => runMigrationsOn(arg, list));
  return Promise.resolve(arg ?? {}).then((options) => buildTime().migrate(options));
}
