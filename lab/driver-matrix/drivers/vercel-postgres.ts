// @vercel/postgres through the local Neon proxy (porg-dm-neon-proxy, in front of the porg-dm-neon-pg
// container): its `sql` (also exported as `db`), the pool of createPool(), a connected client of
// createClient(), and a client checked out of the pool, alone and inside BEGIN/COMMIT. It runs on its
// own copy of @neondatabase/serverless (0.9), whose neonConfig points it at the proxy.
import { createRequire } from "node:module";
import { args, lazy, manualTx, notRun, requireFrom, runJob } from "../lib/harness.ts";

const a = args();
const meta = { pkg: "@vercel/postgres", version: a.version!, target: "Postgres 14 via local Neon proxy" };
// createPool() and `sql` take only a pooled connection string, whose host has "-pooler.", and
// createClient() only a direct one. The proxy knows the endpoint by any host name, and nothing
// resolves either, because wsProxy sends every connection to the proxy's local port.
const pooled = "postgres://postgres:postgres@db-pooler.localtest.me:5432/main";
const direct = "postgres://postgres:postgres@db.localtest.me:5432/main";
const proxy = process.env.DM_NEON_PROXY ?? "127.0.0.1:54444";
// `sql` makes its pool from POSTGRES_URL the first time it is used.
process.env.POSTGRES_URL = pooled;

let vercel: any;
let neonConfig: any;
try {
  vercel = requireFrom(a.pkg!);
  neonConfig = createRequire(requireFrom.resolve(a.pkg!))("@neondatabase/serverless").neonConfig;
} catch (e) {
  notRun(meta, `cannot load ${a.pkg}: ${(e as Error).message}`);
  process.exit(0);
}

await runJob(meta, async () => {
  neonConfig.fetchEndpoint = () => `http://${proxy}/sql`;
  neonConfig.useSecureWebSocket = false;
  neonConfig.wsProxy = () => `${proxy}/v1`;
  const { sql, createPool, createClient } = vercel;
  const pool = createPool({ connectionString: pooled, max: 10 });
  const client = lazy(async () => {
    const c = createClient({ connectionString: direct });
    await c.connect();
    return c;
  });
  const checkout = async () => {
    const c = await pool.connect();
    return { client: c, release: () => c.release() };
  };
  return {
    ...meta,
    dialect: "postgres",
    count: async (tag) => (await pool.query("select count(*)::int as n from driver_matrix.scratch where tag = $1", [tag])).rows[0].n,
    shapes: [
      { name: "sql (also exported as db)", kind: "async", scope: (fn) => fn(sql) },
      { name: "createPool()", kind: "async", scope: (fn) => fn(pool) },
      { name: "createClient() (connected)", kind: "async", scope: async (fn) => fn(await client()) },
      {
        name: "pool client (pool.connect())",
        kind: "async",
        scope: async (fn) => {
          const { client: c, release } = await checkout();
          try {
            await fn(c);
          } finally {
            release();
          }
        },
      },
      {
        name: "pool client in BEGIN/COMMIT",
        kind: "async",
        inTransaction: true,
        scope: async (fn) => {
          const { client: c, release } = await checkout();
          try {
            await c.query("BEGIN");
            await fn(c);
            await c.query("COMMIT");
          } catch (e) {
            await c.query("ROLLBACK").catch(() => {});
            throw e;
          } finally {
            release();
          }
        },
        tx: manualTx({
          open: checkout,
          begin: (c) => c.query("BEGIN"),
          commit: (c) => c.query("COMMIT"),
          rollback: (c) => c.query("ROLLBACK"),
          exec: (c, text) => c.query(text),
        }),
      },
    ],
    close: async () => {
      await pool.end();
      await sql.end();
      await (await client()).end();
    },
  };
});
