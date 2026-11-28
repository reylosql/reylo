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

test("a Postgres module passes the drivers its project installs, in the order they are tried, and the package.json files that say so", () => {
  const dir = project(["pg"]);
  const pg = installedDrivers("postgres", dir)!;
  assert.deepEqual(pg.drivers, ["pgDriver"]);
  assert.ok(pg.watched.includes(resolve(dir, "..", "..", "package.json")), pg.watched.join(", "));
  assert.deepEqual(installedDrivers("postgres", project(["drizzle-orm", "postgres", "@electric-sql/pglite"]))!.drivers, ["drizzleDriver", "drizzlePostgresDriver", "pgliteDriver", "postgresJsDriver"]);
  // Neon's Pool and Client are pg's shape, and its neon() function its own.
  assert.deepEqual(installedDrivers("postgres", project(["@neondatabase/serverless"]))!.drivers, ["pgDriver", "neonDriver"]);
  // Bun.sql has no package: a Bun lockfile says the app runs on Bun.
  assert.deepEqual(installedDrivers("postgres", project(["knex", "pg"], { "bun.lock": "" }))!.drivers, ["knexDriver", "pgDriver", "bunPostgresDriver"]);
});

test("a MySQL or SQLite module passes its project's drivers, node:sqlite's way always, and Cloudflare's where wrangler or its config is there", () => {
  assert.deepEqual(installedDrivers("mysql", project(["mysql2"]))!.drivers, ["mysql2Driver"]);
  assert.deepEqual(installedDrivers("mysql", project(["mariadb", "@planetscale/database", "drizzle-orm"]))!.drivers, ["drizzleDriver", "mariadbDriver", "planetscaleDriver"]);
  assert.deepEqual(installedDrivers("sqlite", project(["better-sqlite3"]))!.drivers, ["syncSqliteDriver"]);
  assert.deepEqual(installedDrivers("sqlite", project(["@libsql/client", "sqlite3"]))!.drivers, ["syncSqliteDriver", "libsqlDriver", "sqlite3Driver"]);
  assert.deepEqual(installedDrivers("sqlite", project([], { "wrangler.jsonc": "{}" }))!.drivers, ["durableObjectDriver", "syncSqliteDriver", "d1Driver"]);
});

test("with no node_modules above the file, nothing says which drivers the app has, and the module passes every one", () => {
  const root = mkdtempSync(join(tmpdir(), "reylo-drivers-"));
  assert.equal(installedDrivers("postgres", root)!.drivers.length, 11);
});
