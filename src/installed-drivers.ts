// The drivers a project installs, which a module a build step writes passes its define function
// (src/loader.ts), so that an app's bundle holds the code of the drivers it can run on and no
// other's. Each driver is found by the packages that bring it: a package is installed where a
// `node_modules` beside the `.sql` file, or above it, holds it, as Node resolves a bare import from
// there. A runtime's own driver has no package: Bun.sql's is found by the build running on Bun or a
// Bun lockfile above the file, Node's `node:sqlite` is always there, and a Durable Object's storage
// and D1 are found by wrangler or its config. Where no `node_modules` stands above the file or the
// build's directory (Yarn's Plug'n'Play, Deno), or none holds a driver of the dialect, nothing says
// which drivers the app has, and the module passes every one.
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import type { Dialect } from "./execute.ts";

/** Bun's own drivers, which no package brings. */
const BUN = Symbol("bun");
/** Node's own SQLite, `node:sqlite`, which no package brings, so every SQLite module passes its driver. */
const NODE = Symbol("node");
/** Cloudflare's own SQLite, a Durable Object's storage and D1, which no package brings: a project on Workers has wrangler, or its config. */
const CLOUDFLARE = Symbol("cloudflare");

/** Each dialect's drivers, as "reylo/module" exports them, in the order they are tried, each with the packages any one of which brings it. */
const DRIVERS: Partial<Record<Dialect, readonly (readonly [driver: string, from: readonly (string | symbol)[]])[]>> = {
  postgres: [
    ["effectDriver", ["effect"]],
    ["slonikDriver", ["slonik"]],
    ["drizzleDriver", ["drizzle-orm"]],
    ["drizzlePostgresDriver", ["drizzle-orm"]],
    ["knexDriver", ["knex"]],
    ["pgPromiseDriver", ["pg-promise"]],
    ["pgliteDriver", ["@electric-sql/pglite"]],
    // Neon's Pool and Client and @vercel/postgres's are pg's shape.
    ["pgDriver", ["pg", "@neondatabase/serverless", "@vercel/postgres"]],
    ["neonDriver", ["@neondatabase/serverless", "@vercel/postgres"]],
    ["postgresJsDriver", ["postgres"]],
    ["bunPostgresDriver", [BUN]],
  ],
  mysql: [
    ["drizzleDriver", ["drizzle-orm"]],
    ["knexDriver", ["knex"]],
    ["bunMysqlDriver", [BUN]],
    ["mariadbDriver", ["mariadb"]],
    ["mysql2Driver", ["mysql2"]],
    ["planetscaleDriver", ["@planetscale/database"]],
  ],
  sqlite: [
    ["drizzleDriver", ["drizzle-orm"]],
    ["knexDriver", ["knex"]],
    ["effectSqliteDriver", ["effect"]],
    ["durableObjectDriver", [CLOUDFLARE]],
    ["sqliteWasmDriver", ["@sqlite.org/sqlite-wasm"]],
    ["sqlJsDriver", ["sql.js"]],
    // node:sqlite has no package, and better-sqlite3, bun:sqlite and Turso's compat Database share its way.
    ["syncSqliteDriver", [NODE, "better-sqlite3"]],
    ["bunSqliteDriver", [BUN]],
    ["libsqlDriver", ["@libsql/client"]],
    ["tursoDriver", ["@tursodatabase/database"]],
    ["sqlitePackageDriver", ["sqlite"]],
    ["sqlite3Driver", ["sqlite3"]],
    ["d1Driver", [CLOUDFLARE]],
  ],
};

/**
 * The drivers of `dialect` the project of a `.sql` file in `dir` installs, in the order they are
 * tried, and the `package.json` files that say so, which a bundler watches, so that the module is
 * written again when the app installs another driver. Undefined for a dialect whose module names no
 * drivers yet.
 */
export function installedDrivers(dialect: Dialect, dir: string, cwd = process.cwd()): { drivers: string[]; watched: string[] } | undefined {
  const all = DRIVERS[dialect];
  if (!all) return undefined;
  const roots = new Set<string>();
  const watched: string[] = [];
  let bunLock = false;
  let workers = false;
  let beside = false;
  // Above the `.sql` file, and above the directory the build runs in, which in a workspace is the
  // app's package, whose own install may hold the driver the package of the queries does not list.
  for (const start of [dir, cwd]) {
    if (start === cwd && !beside) break;
    for (let d = start; ; d = dirname(d)) {
      if (existsSync(join(d, "node_modules"))) {
        roots.add(join(d, "node_modules"));
        if (start === dir) beside = true;
      }
      if (existsSync(join(d, "package.json")) && !watched.includes(join(d, "package.json"))) watched.push(join(d, "package.json"));
      if (existsSync(join(d, "bun.lock")) || existsSync(join(d, "bun.lockb"))) bunLock = true;
      if (["wrangler.toml", "wrangler.json", "wrangler.jsonc"].some((f) => existsSync(join(d, f)))) workers = true;
      if (dirname(d) === d) break;
    }
  }
  const every = { drivers: all.map(([driver]) => driver), watched };
  if (!roots.size) return every;
  const bun = bunLock || typeof (process.versions as { bun?: string }).bun === "string";
  const installed = (name: string) => [...roots].some((root) => existsSync(join(root, name, "package.json")));
  const has = (from: string | symbol) => (from === BUN ? bun : from === NODE ? true : from === CLOUDFLARE ? workers || installed("wrangler") || installed("@cloudflare/workers-types") : installed(from as string));
  const found = all.filter(([, from]) => from.some(has));
  // Nothing but node:sqlite, which is always there: nothing says which driver the app has, and it is given every one.
  if (!found.some(([, from]) => from.some((f) => f !== NODE && has(f)))) return every;
  return { drivers: found.map(([driver]) => driver), watched };
}
