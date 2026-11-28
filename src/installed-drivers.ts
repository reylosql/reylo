// The drivers a project installs, which a module a build step writes passes its define function
// (src/loader.ts), so that an app's bundle holds the code of the drivers it can run on and no
// other's. Each driver is found by the packages that bring it: a package is installed where a
// `node_modules` beside the `.sql` file, or above it, holds it, as Node resolves a bare import from
// there. A runtime's own driver has no package: Bun.sql's is found by the build running on Bun or a
// Bun lockfile above the file. Where no `node_modules` stands above the file at all (Yarn's Plug'n'Play,
// Deno), nothing says which drivers the app has, and the module passes every one.
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import type { Dialect } from "./execute.ts";

/** Bun's own drivers, which no package brings. */
const BUN = Symbol("bun");

/** Each dialect's drivers, as "reylo/module" exports them, in the order they are tried, each with the packages any one of which brings it. */
const DRIVERS: Partial<Record<Dialect, readonly (readonly [driver: string, from: readonly (string | typeof BUN)[]])[]>> = {
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
};

/**
 * The drivers of `dialect` the project of a `.sql` file in `dir` installs, in the order they are
 * tried, and the `package.json` files that say so, which a bundler watches, so that the module is
 * written again when the app installs another driver. Undefined for a dialect whose module names no
 * drivers yet.
 */
export function installedDrivers(dialect: Dialect, dir: string): { drivers: string[]; watched: string[] } | undefined {
  const all = DRIVERS[dialect];
  if (!all) return undefined;
  const roots: string[] = [];
  const watched: string[] = [];
  let bunLock = false;
  for (let d = dir; ; d = dirname(d)) {
    if (existsSync(join(d, "node_modules"))) roots.push(join(d, "node_modules"));
    if (existsSync(join(d, "package.json"))) watched.push(join(d, "package.json"));
    if (existsSync(join(d, "bun.lock")) || existsSync(join(d, "bun.lockb"))) bunLock = true;
    if (dirname(d) === d) break;
  }
  if (!roots.length) return { drivers: all.map(([driver]) => driver), watched };
  const bun = bunLock || typeof (process.versions as { bun?: string }).bun === "string";
  const has = (from: string | typeof BUN) => (from === BUN ? bun : roots.some((root) => existsSync(join(root, from, "package.json"))));
  return { drivers: all.filter(([, from]) => from.some(has)).map(([driver]) => driver), watched };
}
