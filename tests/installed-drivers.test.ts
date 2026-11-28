// The drivers a module passes, by the packages its project installs (src/installed-drivers.ts).
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { installedDrivers } from "../src/installed-drivers.ts";

/** A project's root at a new directory, with a lockfile, an empty package of each name in its `node_modules`, and `files` beside them; at `at`, a package of a workspace, with no lockfile of its own. */
function root(packages: string[], files: Record<string, string> = {}, at?: string): string {
  const dir = at ?? mkdtempSync(join(tmpdir(), "reylo-drivers-"));
  mkdirSync(join(dir, "node_modules"), { recursive: true });
  writeFileSync(join(dir, "package.json"), files["package.json"] ?? "{}");
  if (at === undefined && !files["pnpm-workspace.yaml"]) writeFileSync(join(dir, "package-lock.json"), "{}");
  for (const name of packages) {
    mkdirSync(join(dir, "node_modules", name), { recursive: true });
    writeFileSync(join(dir, "node_modules", name, "package.json"), "{}");
  }
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  return dir;
}

/** The drivers a module of a `.sql` file in a project with `packages` installed passes. */
function scanned(dialect: "postgres" | "mysql" | "sqlite", packages: string[], files?: Record<string, string>): string[] {
  const dir = join(root(packages, files), "src", "queries");
  mkdirSync(dir, { recursive: true });
  return installedDrivers(dialect, dir)!.drivers;
}

test("a module passes the drivers its project installs, in the order they are tried", () => {
  assert.deepEqual(scanned("postgres", ["pg"]), ["pgDriver"]);
  assert.deepEqual(scanned("postgres", ["drizzle-orm", "postgres", "@electric-sql/pglite"]), ["drizzlePostgresDriver", "pgliteDriver", "postgresJsDriver"]);
  // Neon's Pool and Client are pg's shape, and its neon() function its own; pg-promise hands out pg's Pool.
  assert.deepEqual(scanned("postgres", ["@neondatabase/serverless"]), ["pgDriver", "neonDriver"]);
  assert.deepEqual(scanned("postgres", ["pg-promise"]), ["pgPromiseDriver", "pgDriver"]);
  assert.deepEqual(scanned("mysql", ["mysql2"]), ["mysql2Driver"]);
  assert.deepEqual(scanned("mysql", ["mariadb", "@planetscale/database", "knex"]), ["knexDriver", "mariadbDriver", "planetscaleDriver"]);
  // Effect's MySQL client runs on mysql2's connection.
  assert.deepEqual(scanned("mysql", ["effect", "@effect/sql-mysql2"]), ["mysql2Driver"]);
  assert.deepEqual(scanned("sqlite", ["better-sqlite3"]), ["syncSqliteDriver"]);
  assert.deepEqual(scanned("sqlite", ["@libsql/client", "sqlite3"]), ["syncSqliteDriver", "libsqlDriver", "sqlite3Driver"]);
});

test("Bun's drivers by its lockfile or its types, and Cloudflare's by wrangler or its config", () => {
  assert.deepEqual(scanned("postgres", ["pg"], { "bun.lock": "" }), ["pgDriver", "bunPostgresDriver"]);
  assert.deepEqual(scanned("postgres", ["pg", "@types/bun"]), ["pgDriver", "bunPostgresDriver"]);
  assert.deepEqual(scanned("sqlite", ["@libsql/client"], { "wrangler.jsonc": "{}" }), ["durableObjectDriver", "syncSqliteDriver", "libsqlDriver", "d1Driver"]);
});

test("where no driver's own package is installed, nothing says which driver the app has, and the module passes every one", () => {
  assert.equal(scanned("postgres", ["left-pad"]).length, 10);
  // A wrapper, a runtime and node:sqlite name no driver.
  assert.equal(scanned("postgres", ["drizzle-orm", "knex", "effect"], { "bun.lock": "" }).length, 10);
  assert.equal(scanned("sqlite", ["wrangler"]).length, 12);
  // Nor does a project with no node_modules at all, as on Yarn's Plug'n'Play or Deno.
  const bare = mkdtempSync(join(tmpdir(), "reylo-drivers-"));
  writeFileSync(join(bare, "deno.json"), "{}");
  assert.equal(installedDrivers("postgres", bare)!.drivers.length, 10);
});

test("a workspace's packages are read: the app's install holds the driver the package of its queries lists none of", () => {
  const top = root([], { "pnpm-workspace.yaml": "packages:\n  - packages/*\n  - 'apps/*'\n" });
  root(["@electric-sql/pglite"], { "package.json": '{"name":"db"}' }, join(top, "packages", "db"));
  root(["pg"], { "package.json": '{"name":"web"}' }, join(top, "apps", "web"));
  const queries = join(top, "packages", "db", "queries");
  mkdirSync(queries, { recursive: true });
  const found = installedDrivers("postgres", queries)!;
  // The queries' tests run on PGlite, and the app on pg.
  assert.deepEqual(found.drivers, ["pgliteDriver", "pgDriver"]);
  assert.ok(found.watched.includes(join(top, "apps", "web", "package.json")));
  // package.json's workspaces too.
  const npm = root([], { "package.json": '{"workspaces":["apps/*"]}' });
  root(["mysql2"], { "package.json": '{"name":"web"}' }, join(npm, "apps", "web"));
  mkdirSync(join(npm, "queries"));
  assert.deepEqual(installedDrivers("mysql", join(npm, "queries"))!.drivers, ["mysql2Driver"]);
});

test("reylo generate reads the install of the directory it writes to as well", () => {
  const queries = join(root([]), "queries");
  mkdirSync(queries);
  const app = root(["postgres"]);
  assert.deepEqual(installedDrivers("postgres", queries, [app])!.drivers, ["postgresJsDriver"]);
});
