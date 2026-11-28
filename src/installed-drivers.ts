// The drivers a project installs, which a module a build step writes passes its define function
// (src/loader.ts), so that an app's bundle holds the code of the drivers it can run on and no
// other's. Each driver is found by the packages that bring it, installed in any `node_modules` of
// the project: above the `.sql` file and the file a build writes, up to the project's root, and in each package of the workspace that root is, where a workspace package of
// queries alone lists no driver and the app's own package lists it.
//
// A runtime's own driver has no package: Bun's is found by its lockfile or its types, Node's
// `node:sqlite` is always there, and a Durable Object's storage and D1 by wrangler or its config.
// Where the project installs no package of a driver of the dialect (Yarn's Plug'n'Play, Deno, or a
// project that installs only a wrapper such as drizzle-orm), nothing says which drivers the app
// has, and the module passes every one.
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import type { Dialect } from "./execute.ts";

/** Bun's own drivers, which no package brings: a Bun lockfile or Bun's types say the app runs on Bun. */
const BUN = Symbol("bun");
/** Node's own SQLite, `node:sqlite`, which no package brings, so every SQLite module passes its driver. */
const NODE = Symbol("node");
/** Cloudflare's own SQLite, a Durable Object's storage and D1, which no package brings: a project on Workers has wrangler, or its config. */
const CLOUDFLARE = Symbol("cloudflare");

/** A driver as "reylo/module" exports it, the packages and runtimes any one of which brings it, and whether it is a wrapper over another driver, which says nothing of which one the app has. */
type Entry = readonly [driver: string, from: readonly (string | symbol)[], wrapper?: true];

/** Each dialect's drivers, in the order they are tried. */
const DRIVERS: Partial<Record<Dialect, readonly Entry[]>> = {
  postgres: [
    ["effectDriver", ["effect"], true],
    ["slonikDriver", ["slonik"]],
    ["drizzlePostgresDriver", ["drizzle-orm"], true],
    ["knexDriver", ["knex"], true],
    ["pgPromiseDriver", ["pg-promise"]],
    ["pgliteDriver", ["@electric-sql/pglite"]],
    // Neon's Pool and Client and @vercel/postgres's are pg's shape, and pg-promise hands out pg's Pool (db.$pool).
    ["pgDriver", ["pg", "@neondatabase/serverless", "@vercel/postgres", "pg-promise"]],
    ["neonDriver", ["@neondatabase/serverless", "@vercel/postgres"]],
    ["postgresJsDriver", ["postgres"]],
    ["bunPostgresDriver", [BUN]],
  ],
  mysql: [
    ["knexDriver", ["knex"], true],
    ["bunMysqlDriver", [BUN]],
    ["mariadbDriver", ["mariadb"]],
    // Effect SQL's MySQL client runs on mysql2's connection (src/effect.ts).
    ["mysql2Driver", ["mysql2", "@effect/sql-mysql2"]],
    ["planetscaleDriver", ["@planetscale/database"]],
  ],
  sqlite: [
    ["knexDriver", ["knex"], true],
    ["effectSqliteDriver", ["effect"], true],
    ["durableObjectDriver", [CLOUDFLARE]],
    ["sqliteWasmDriver", ["@sqlite.org/sqlite-wasm"]],
    ["sqlJsDriver", ["sql.js", "sql.js-fts5"]],
    // node:sqlite has no package, and better-sqlite3, bun:sqlite, libsql's Database and Turso's compat Database share its way.
    ["syncSqliteDriver", [NODE, "better-sqlite3", "libsql", "@tursodatabase/database", "@tursodatabase/database-wasm"]],
    ["bunSqliteDriver", [BUN]],
    ["libsqlDriver", ["@libsql/client", "@libsql/client-wasm"]],
    ["tursoDriver", ["@tursodatabase/database", "@tursodatabase/database-wasm", "@tursodatabase/sync"]],
    ["sqlitePackageDriver", ["sqlite"]],
    ["sqlite3Driver", ["sqlite3"]],
    ["d1Driver", [CLOUDFLARE]],
  ],
};

/** The files that mark a project's root: an ancestor past it is no part of the project. */
const ROOT_MARKS = ["package-lock.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", "yarn.lock", "bun.lock", "bun.lockb", "deno.json", "deno.jsonc", ".git"];
const BUN_LOCKS = ["bun.lock", "bun.lockb"];
const WRANGLER = ["wrangler.toml", "wrangler.json", "wrangler.jsonc"];

/**
 * The drivers of `dialect` the project of a `.sql` file in `dir` installs, in the order they are
 * tried, and the files that say so, which a bundler watches, so that the module is written again
 * when the app installs another driver. `places` are other directories of the project the build
 * reads, as the one `reylo generate` writes to. Undefined for a dialect whose module names no
 * drivers.
 */
export function installedDrivers(dialect: Dialect, dir: string, places: readonly string[] = []): { drivers: string[]; watched: string[] } | undefined {
  const all = DRIVERS[dialect];
  if (!all) return undefined;
  const roots = new Set<string>();
  const watched = new Set<string>();
  let bun = false;
  let workers = false;
  /** Reads `d` for installs and the files that say which runtime the app has. */
  const read = (d: string) => {
    if (existsSync(join(d, "node_modules"))) roots.add(join(d, "node_modules"));
    for (const f of ["package.json", ...BUN_LOCKS, ...WRANGLER]) {
      if (!existsSync(join(d, f))) continue;
      watched.add(join(d, f));
      if (BUN_LOCKS.includes(f)) bun = true;
      if (WRANGLER.includes(f)) workers = true;
    }
  };
  const tops = new Set<string>();
  for (const start of [dir, ...places]) {
    for (let d = resolve(start); ; d = dirname(d)) {
      read(d);
      if (ROOT_MARKS.some((m) => existsSync(join(d, m))) || dirname(d) === d) {
        tops.add(d);
        break;
      }
    }
  }
  // Each package of a workspace, whose own install may hold the app's driver.
  for (const top of tops) for (const pkg of workspacePackages(top)) read(pkg);
  const installed = (name: string) => [...roots].some((root) => existsSync(join(root, name, "package.json")));
  if (installed("@types/bun") || installed("bun-types")) bun = true;
  const has = (from: string | symbol) => (from === BUN ? bun : from === NODE ? true : from === CLOUDFLARE ? workers || installed("wrangler") || installed("@cloudflare/workers-types") : installed(from as string));
  const found = all.filter(([, from]) => from.some(has));
  // A driver's own package says which driver the app has; a wrapper, a runtime and node:sqlite do not.
  const evidence = found.some(([, from, wrapper]) => !wrapper && from.some((f) => typeof f === "string" && has(f)));
  return { drivers: (evidence ? found : all).map(([driver]) => driver), watched: [...watched] };
}

/** The package directories of the workspace whose root is `top`: from pnpm-workspace.yaml's `packages`, or package.json's `workspaces`, each pattern a directory or one ending in `/*` or `/**`. */
function workspacePackages(top: string): string[] {
  const patterns: string[] = [];
  try {
    const yaml = readFileSync(join(top, "pnpm-workspace.yaml"), "utf8");
    const list = /^packages:\s*\n((?:\s+-\s*.+\n?)+)/m.exec(yaml)?.[1] ?? "";
    for (const m of list.matchAll(/-\s*["']?([^"'\n#]+?)["']?\s*(?:#.*)?$/gm)) patterns.push(m[1]!.trim());
  } catch {}
  try {
    const pkg = JSON.parse(readFileSync(join(top, "package.json"), "utf8")) as { workspaces?: string[] | { packages?: string[] } };
    const ws = Array.isArray(pkg.workspaces) ? pkg.workspaces : (pkg.workspaces?.packages ?? []);
    patterns.push(...ws.filter((w) => typeof w === "string"));
  } catch {}
  const out: string[] = [];
  for (const pattern of patterns) {
    if (pattern.startsWith("!")) continue;
    const base = pattern.replace(/\/\*\*?$/, "");
    if (base.includes("*")) continue;
    if (base === pattern) out.push(join(top, base));
    else out.push(...subdirectories(join(top, base), pattern.endsWith("**") ? 3 : 1));
  }
  return out.filter((d) => existsSync(join(d, "package.json")));
}

/** The directories under `dir`, `depth` levels down at most, past any `node_modules`. */
function subdirectories(dir: string, depth: number): string[] {
  if (depth === 0) return [];
  let names: string[];
  try {
    names = readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory() && e.name !== "node_modules" && !e.name.startsWith(".")).map((e) => e.name);
  } catch {
    return [];
  }
  return names.flatMap((name) => [join(dir, name), ...subdirectories(join(dir, name), depth - 1)]);
}
