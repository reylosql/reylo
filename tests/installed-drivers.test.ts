// The drivers a module passes, by the packages its project installs (src/installed-drivers.ts).
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { installedDrivers } from "../src/installed-drivers.ts";

/** A project at a new directory, with an empty package of each name in its `node_modules`, and the `.sql` file's directory inside it. */
function project(packages: string[], files: Record<string, string> = {}): string {
  const root = mkdtempSync(join(tmpdir(), "reylo-drivers-"));
  writeFileSync(join(root, "package.json"), "{}");
  mkdirSync(join(root, "node_modules"));
  for (const name of packages) {
    mkdirSync(join(root, "node_modules", name), { recursive: true });
    writeFileSync(join(root, "node_modules", name, "package.json"), "{}");
  }
  for (const [name, text] of Object.entries(files)) writeFileSync(join(root, name), text);
  mkdirSync(join(root, "src", "queries"), { recursive: true });
  return join(root, "src", "queries");
}

/** The drivers a module of `dialect` in `dir` passes, built from the project's own root. */
const scanned = (dialect: "postgres" | "mysql" | "sqlite", dir: string) => installedDrivers(dialect, dir, resolve(dir, "..", ".."))!;

test("a Postgres module passes the drivers its project installs, in the order they are tried, and the package.json files that say so", () => {
  const dir = project(["pg"]);
  const pg = scanned("postgres", dir);
  assert.deepEqual(pg.drivers, ["pgDriver"]);
  assert.ok(pg.watched.includes(resolve(dir, "..", "..", "package.json")), pg.watched.join(", "));
  assert.deepEqual(scanned("postgres", project(["drizzle-orm", "postgres", "@electric-sql/pglite"])).drivers, ["drizzleDriver", "drizzlePostgresDriver", "pgliteDriver", "postgresJsDriver"]);
  // Neon's Pool and Client are pg's shape, and its neon() function its own.
  assert.deepEqual(scanned("postgres", project(["@neondatabase/serverless"])).drivers, ["pgDriver", "neonDriver"]);
  // Bun.sql has no package: a Bun lockfile says the app runs on Bun.
  assert.deepEqual(scanned("postgres", project(["knex", "pg"], { "bun.lock": "" })).drivers, ["knexDriver", "pgDriver", "bunPostgresDriver"]);
});

test("a MySQL or SQLite module passes its project's drivers, node:sqlite's way always, and Cloudflare's where wrangler or its config is there", () => {
  assert.deepEqual(scanned("mysql", project(["mysql2"])).drivers, ["mysql2Driver"]);
  assert.deepEqual(scanned("mysql", project(["mariadb", "@planetscale/database", "drizzle-orm"])).drivers, ["drizzleDriver", "mariadbDriver", "planetscaleDriver"]);
  assert.deepEqual(scanned("sqlite", project(["better-sqlite3"])).drivers, ["syncSqliteDriver"]);
  assert.deepEqual(scanned("sqlite", project(["@libsql/client", "sqlite3"])).drivers, ["syncSqliteDriver", "libsqlDriver", "sqlite3Driver"]);
  assert.deepEqual(scanned("sqlite", project([], { "wrangler.jsonc": "{}" })).drivers, ["durableObjectDriver", "syncSqliteDriver", "d1Driver"]);
});

test("with no node_modules above the file, nothing says which drivers the app has, and the module passes every one", () => {
  const root = mkdtempSync(join(tmpdir(), "reylo-drivers-"));
  assert.equal(installedDrivers("postgres", root, root)!.drivers.length, 11);
});

test("an install that holds no driver of the dialect, as a workspace package of queries alone, passes every one; the build's own directory finds the app's", () => {
  assert.equal(scanned("postgres", project(["left-pad"])).drivers.length, 11);
  assert.equal(scanned("sqlite", project([])).drivers.length, 13);
  // The queries' package lists no driver, and the app's package, where the build runs, lists pg.
  const queries = project([]);
  const app = resolve(project(["pg"]), "..", "..");
  assert.deepEqual(installedDrivers("postgres", queries, app)!.drivers, ["pgDriver"]);
});
