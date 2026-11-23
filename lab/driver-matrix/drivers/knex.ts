// Knex over pg against the shared local Postgres: the instance, a transaction from
// knex.transaction(trx) and a savepoint trx.transaction(sp) inside it, and the transaction
// `await knex.transaction()` hands out to commit or roll back by hand.
import { args, callbackTx, manualTx, notRun, requireFrom, runJob } from "../lib/harness.ts";

const a = args();
const meta = { pkg: "knex (pg)", version: a.version!, target: "Postgres 14 (local)" };
const url = process.env.DM_PG_URL ?? "postgres://localhost:5432/postgres";

let knex: any;
try {
  knex = requireFrom(a.pkg!);
} catch (e) {
  notRun(meta, `cannot load ${a.pkg}: ${(e as Error).message}`);
  process.exit(0);
}

await runJob(meta, async () => {
  const db = knex({ client: "pg", connection: url, pool: { min: 0, max: 10 } });
  const begin = (fn: (trx: any) => Promise<unknown>) => db.transaction(fn);
  // A transaction to commit or roll back by hand; rollback() rejects the transaction's own promise,
  // which nothing here awaits. Its release rolls back a transaction a failed scenario left open, so
  // its connection goes back to the pool.
  const handed = async () => {
    const trx = await db.transaction();
    trx.executionPromise?.catch(() => {});
    return { client: trx, release: async () => { if (!trx.isCompleted()) await trx.rollback(); } };
  };
  return {
    ...meta,
    dialect: "postgres",
    count: async (tag) => (await db.raw("select count(*)::int as n from driver_matrix.scratch where tag = ?", [tag])).rows[0].n,
    shapes: [
      { name: "knex instance", kind: "async", scope: (fn) => fn(db) },
      {
        name: "knex.transaction(trx)",
        kind: "async",
        inTransaction: true,
        scope: (fn) => begin((trx) => fn(trx)),
        tx: callbackTx(begin, (trx, fn) => trx.transaction(fn)),
      },
      {
        name: "trx.transaction(sp) (savepoint)",
        kind: "async",
        inTransaction: true,
        scope: (fn) => begin((trx) => trx.transaction((sp: any) => fn(sp))),
        tx: callbackTx(
          (fn) => begin((trx) => trx.transaction(fn)),
          (sp, fn) => sp.transaction(fn),
        ),
      },
      {
        name: "await knex.transaction() (by hand)",
        kind: "async",
        inTransaction: true,
        scope: async (fn) => {
          const { client: trx } = await handed();
          try {
            await fn(trx);
            await trx.commit();
          } catch (e) {
            await trx.rollback().catch(() => {});
            throw e;
          }
        },
        tx: manualTx({
          open: handed,
          begin: () => {},
          commit: (trx) => trx.commit(),
          rollback: (trx) => trx.rollback(),
          exec: (trx, text) => trx.raw(text),
        }),
      },
    ],
    close: () => db.destroy(),
  };
});
