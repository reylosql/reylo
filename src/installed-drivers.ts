// The drivers an app depends on, which a module a build step writes passes its define function
// (src/loader.ts), so that the app's bundle holds the code of the drivers it can run on and no
// other's. They are read from what the app being built declares, its package.json's dependencies of
// every kind; from the `.sql` file's own package; from each workspace package either depends on,
// followed by name through the workspace; and from the workspace's root. The app is the package the
// bundler builds, its root. Leaving out a driver the app runs on refuses its queries, where taking
// in one too many costs only its bytes, so the list is narrowed only where a runtime dependency names
// a driver's own package.
//
// Bun's drivers have no package: Bun's lockfile, its types or a `packageManager` naming Bun say the
// app runs on Bun. Node's `node:sqlite` and Cloudflare's SQLite (a Durable Object's storage and
// D1) have none either, and are small, so every SQLite module passes them. An app that declares no
// driver's own package of the dialect as a runtime dependency (one that declares only a wrapper such
// as drizzle-orm, or a driver its tests use, or no package.json at all) says nothing of which driver
// it has, and its modules pass every one, as does a build rooted at a workspace's root.
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import type { Dialect } from "./execute.ts";

/** Bun's own drivers, which no package brings. */
const BUN = Symbol("bun");
/** A runtime's own SQLite with no package, small enough that every SQLite module passes it: node:sqlite, a Durable Object's storage and D1. */
const ALWAYS = Symbol("always");

/** A driver as "reylo/module" exports it, the packages any one of which brings it, and whether it is a wrapper over another driver, which says nothing of which one the app has. */
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
    ["pgDriver", ["pg", "@neondatabase/serverless", "@vercel/postgres", "@neondatabase/vercel-postgres-compat", "pg-promise", "pg-mem", "@prisma/adapter-pg", "@prisma/adapter-neon", "@payloadcms/db-postgres", "@payloadcms/db-vercel-postgres"]],
    ["neonDriver", ["@neondatabase/serverless", "@vercel/postgres", "@neondatabase/vercel-postgres-compat", "@prisma/adapter-neon"]],
    ["postgresJsDriver", ["postgres"]],
    ["bunPostgresDriver", [BUN]],
  ],
  mysql: [
    ["knexDriver", ["knex"], true],
    ["bunMysqlDriver", [BUN]],
    ["mariadbDriver", ["mariadb", "@prisma/adapter-mariadb"]],
    // Effect SQL's MySQL client runs on mysql2's connection (src/effect.ts).
    ["mysql2Driver", ["mysql2", "@effect/sql-mysql2"]],
    ["planetscaleDriver", ["@planetscale/database", "@prisma/adapter-planetscale"]],
  ],
  sqlite: [
    ["knexDriver", ["knex"], true],
    ["effectSqliteDriver", ["effect"], true],
    ["durableObjectDriver", [ALWAYS]],
    ["sqliteWasmDriver", ["@sqlite.org/sqlite-wasm"]],
    ["sqlJsDriver", ["sql.js", "sql.js-fts5"]],
    // node:sqlite's way, which better-sqlite3, bun:sqlite, libsql's Database and Turso's compat Database share.
    ["syncSqliteDriver", [ALWAYS, "better-sqlite3", "libsql", "@tursodatabase/database", "@tursodatabase/database-wasm", "@prisma/adapter-better-sqlite3"]],
    ["bunSqliteDriver", [BUN]],
    // Astro DB's `db` and Payload's SQLite adapter run on libSQL's client.
    ["libsqlDriver", ["@libsql/client", "@libsql/client-wasm", "@prisma/adapter-libsql", "@astrojs/db", "@payloadcms/db-sqlite"]],
    ["tursoDriver", ["@tursodatabase/database", "@tursodatabase/database-wasm", "@tursodatabase/sync"]],
    ["sqlitePackageDriver", ["sqlite"]],
    ["sqlite3Driver", ["sqlite3", "@vscode/sqlite3"]],
    ["d1Driver", [ALWAYS]],
  ],
};

type Manifest = { name?: string; packageManager?: string; engines?: Record<string, string>; workspaces?: string[] | { packages?: string[] }; dependencies?: Record<string, string>; devDependencies?: Record<string, string>; optionalDependencies?: Record<string, string>; peerDependencies?: Record<string, string> };

/** What was read for an app, kept while none of the files it read has changed. */
const cache = new Map<string, { read: Read; stamps: string }>();
/** The packages an app depends on, those of its runtime dependencies apart, and whether it runs on Bun; undefined where nothing says (no package.json, or an app that is a workspace's root). */
type Read = { names: Set<string>; runtime: Set<string>; bun: boolean; watched: string[] } | undefined;

/**
 * The drivers of `dialect` the app at `app` depends on, in the order they are tried, and the files
 * that say so, which a bundler watches, so that the module is written again when the app adds a
 * driver. The package of the `.sql` file in `dir` is read beside the app's. `null` is a module no
 * bundler builds (Node's preload, Jest, `reylo generate`'s file, which runs anywhere), whose size
 * is nothing to anyone: it passes every driver. Undefined for a dialect whose module names no
 * drivers.
 */
export function installedDrivers(dialect: Dialect, app: string | null, dir?: string): { drivers: string[]; watched: string[] } | undefined {
  const all = DRIVERS[dialect];
  if (!all) return undefined;
  const read = app === null ? undefined : readApp(resolve(app), dir === undefined ? undefined : resolve(dir));
  const has = (from: string | symbol) => from === ALWAYS || (from === BUN ? read!.bun : read!.names.has(from as string));
  // A driver's own package among the runtime dependencies says which driver the app runs on; a
  // wrapper, a runtime and a package only its tests or scripts use (a devDependency) do not.
  const evidence = read !== undefined && all.some(([, from, wrapper]) => !wrapper && from.some((f) => typeof f === "string" && read.runtime.has(f)));
  return { drivers: (evidence ? all.filter(([, from]) => from.some(has)) : all).map(([driver]) => driver), watched: read?.watched ?? [] };
}

/** What the app at `app` depends on, with the package of the `.sql` file in `dir` (`Read`), kept while none of the files read has changed. */
function readApp(app: string, dir: string | undefined): Read {
  const key = `${app}\0${dir ?? ""}`;
  const hit = cache.get(key);
  if (hit && hit.stamps === stamps(hit.read?.watched ?? [])) return hit.read;
  const read = readAppNow(app, dir);
  cache.set(key, { read, stamps: stamps(read?.watched ?? []) });
  return read;
}

function readAppNow(app: string, dir: string | undefined): Read {
  const nearest = (from: string): string | undefined => {
    for (let d = from; ; d = dirname(d)) {
      if (existsSync(join(d, "package.json"))) return d;
      if (dirname(d) === d) return undefined;
    }
  };
  const appDir = nearest(app);
  if (appDir === undefined) return undefined;
  const watched = new Set<string>();
  const manifest = (d: string): Manifest | undefined => {
    try {
      const m = JSON.parse(readFileSync(join(d, "package.json"), "utf8")) as Manifest;
      watched.add(join(d, "package.json"));
      return m;
    } catch {
      return undefined;
    }
  };
  const root = workspaceRoot(appDir);
  // A build rooted at a workspace's root builds whichever of its apps it is given, which the root does not say.
  if (root === appDir) return undefined;
  const packages = root ? workspacePackages(root, watched) : new Map<string, string>();
  const names = new Set<string>();
  const runtime = new Set<string>();
  let bun = false;
  const seen = new Set<string>();
  /**
   * Adds every dependency the package at `d` declares, and those of each workspace package it
   * depends on, by name, alias or `workspace:`, `file:` or `link:` spec: a workspace installs a
   * member's devDependencies too, where a bundler resolves its imports.
   */
  const add = (d: string) => {
    if (seen.has(d)) return;
    seen.add(d);
    const m = manifest(d);
    if (!m) return;
    if (m.packageManager?.startsWith("bun@") || m.engines?.bun !== undefined) bun = true;
    for (const [deps, isRuntime] of [[m.dependencies, true], [m.optionalDependencies, true], [m.peerDependencies, true], [m.devDependencies, false]] as const) {
      for (const [key, spec] of Object.entries(deps ?? {})) {
        // An alias, `"pg8": "npm:pg@^8"`, imports as its key and installs the package it names.
        const target = typeof spec === "string" ? /^(?:npm|workspace):((?:@[^/@]+\/)?[^@]+)(?:@|$)/.exec(spec)?.[1] : undefined;
        for (const name of target === undefined ? [key] : [key, target]) {
          names.add(name);
          if (isRuntime) runtime.add(name);
        }
        const local = packages.get(key) ?? (target === undefined ? undefined : packages.get(target)) ?? (typeof spec === "string" && /^(?:file|link):/.test(spec) ? resolve(d, spec.replace(/^(?:file|link):/, "")) : undefined);
        if (local !== undefined) add(local);
      }
    }
  };
  add(appDir);
  // The `.sql` file's own package, which an app may reach by a path rather than a dependency.
  const sqlDir = dir === undefined ? undefined : nearest(dir);
  if (sqlDir !== undefined) add(sqlDir);
  // A workspace's root declares what every package of it resolves.
  if (root !== undefined) add(root);
  if (names.has("@types/bun") || names.has("bun-types")) bun = true;
  for (const d of new Set([appDir, ...(root === undefined ? [] : [root])])) {
    for (const f of ["bun.lock", "bun.lockb", "bunfig.toml"]) {
      if (existsSync(join(d, f))) {
        bun = true;
        watched.add(join(d, f));
      }
    }
  }
  return { names, runtime, bun, watched: [...watched] };
}

/** The root of the workspace `dir` is in: the nearest directory at or above it with a pnpm-workspace.yaml, a package.json that lists workspaces, or a deno.json that does. */
function workspaceRoot(dir: string): string | undefined {
  for (let d = dir; ; d = dirname(d)) {
    if (existsSync(join(d, "pnpm-workspace.yaml"))) return d;
    try {
      if ((JSON.parse(readFileSync(join(d, "package.json"), "utf8")) as Manifest).workspaces !== undefined) return d;
    } catch {}
    for (const deno of ["deno.json", "deno.jsonc"]) {
      try {
        if (/"workspace"\s*:/.test(readFileSync(join(d, deno), "utf8"))) return d;
      } catch {}
    }
    if (dirname(d) === d) return undefined;
  }
}

/** Each package of the workspace at `root`, by its name, from the globs its pnpm-workspace.yaml, package.json or deno.json lists. */
function workspacePackages(root: string, watched: Set<string>): Map<string, string> {
  const globs: string[] = [];
  const text = (f: string) => {
    try {
      const t = readFileSync(join(root, f), "utf8");
      watched.add(join(root, f));
      return t;
    } catch {
      return undefined;
    }
  };
  const yaml = text("pnpm-workspace.yaml");
  if (yaml !== undefined) {
    // `packages:` as a block list, indented or not, comments between its items, or as a flow list.
    const at = yaml.search(/^packages\s*:/m);
    if (at >= 0) {
      const rest = yaml.slice(at).replace(/^packages\s*:/, "");
      const flow = /^\s*\[([^\]]*)\]/.exec(rest);
      if (flow) globs.push(...flow[1]!.split(","));
      else for (const line of rest.split("\n").slice(1)) {
        if (/^\s*(?:#.*)?$/.test(line)) continue;
        const item = /^\s*-\s*(.+?)\s*(?:#.*)?$/.exec(line);
        if (!item) break;
        globs.push(item[1]!);
      }
    }
  }
  for (const f of ["package.json", "deno.json", "deno.jsonc"]) {
    const t = text(f);
    if (t === undefined) continue;
    try {
      const m = JSON.parse(f === "package.json" ? t : t.replace(/^\s*\/\/.*$/gm, "")) as { workspaces?: string[] | { packages?: string[] }; workspace?: string[] };
      const ws = Array.isArray(m.workspaces) ? m.workspaces : (m.workspaces?.packages ?? m.workspace ?? []);
      globs.push(...ws.filter((w): w is string => typeof w === "string"));
    } catch {}
  }
  const patterns = globs.map((g) => g.trim().replace(/^["']|["']$/g, "").replace(/^\.\//, "").replace(/\/+$/, "")).filter((g) => g && !g.startsWith("!"));
  const matchers = patterns.map(globMatcher);
  const out = new Map<string, string>();
  for (const d of directories(root, 5)) {
    const rel = relative(root, d).split(sep).join("/");
    if (!matchers.some((m) => m.test(rel)) || !existsSync(join(d, "package.json"))) continue;
    try {
      const name = (JSON.parse(readFileSync(join(d, "package.json"), "utf8")) as Manifest).name;
      if (name) out.set(name, d);
    } catch {}
  }
  return out;
}

/** A workspace glob as a test of a directory's path from the root: `*` within one level, `**` across any, and `{a,b}` either. */
function globMatcher(glob: string): RegExp {
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]!;
    if (c === "*" && glob[i + 1] === "*") {
      re += ".*";
      i++;
      if (glob[i + 1] === "/") i++;
    } else if (c === "*") re += "[^/]*";
    else if (c === "?") re += "[^/]";
    else if (c === "{") {
      const end = glob.indexOf("}", i);
      re += `(?:${glob.slice(i + 1, end).split(",").map((s) => s.replace(/[.+^$()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*")).join("|")})`;
      i = end;
    } else re += c.replace(/[.+^$()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${re}$`);
}

/** The directories under `dir`, `depth` levels down at most, past `node_modules` and hidden ones. */
function directories(dir: string, depth: number): string[] {
  if (depth === 0) return [];
  let names: string[];
  try {
    names = readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory() && e.name !== "node_modules" && !e.name.startsWith(".")).map((e) => e.name);
  } catch {
    return [];
  }
  return names.flatMap((name) => [join(dir, name), ...directories(join(dir, name), depth - 1)]);
}

/** When each of `files` last changed, so that what was read from them is read again after any changes. */
function stamps(files: readonly string[]): string {
  return files.map((f) => {
    try {
      return statSync(f).mtimeMs;
    } catch {
      return "-";
    }
  }).join(",");
}
