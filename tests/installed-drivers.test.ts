// The drivers a module passes, by what the app being built depends on (src/installed-drivers.ts).
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { installedDrivers } from "../src/installed-drivers.ts";
import * as reyloModule from "../src/module.ts";
import { postgresDrivers } from "../src/decode.ts";
import { mysqlDrivers } from "../src/decode-mysql.ts";
import { sqliteDrivers } from "../src/decode-sqlite.ts";

/** A package at `dir` (a new directory where none is given) declaring `manifest`, with `files` beside it. */
function pkg(manifest: object, files: Record<string, string> = {}, dir = mkdtempSync(join(tmpdir(), "reylo-drivers-"))): string {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify(manifest));
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  return dir;
}

/** The drivers of an app that declares `dependencies` (and `devDependencies`). */
const of = (dialect: "postgres" | "mysql" | "sqlite", dependencies: string[], devDependencies: string[] = [], files?: Record<string, string>) =>
  installedDrivers(dialect, pkg({ dependencies: Object.fromEntries(dependencies.map((d) => [d, "*"])), devDependencies: Object.fromEntries(devDependencies.map((d) => [d, "*"])) }, files))!.drivers;

test("a module passes the drivers its app depends on, in the order they are tried", () => {
  assert.deepEqual(of("postgres", ["pg"]), ["pgDriver"]);
  assert.deepEqual(of("postgres", ["drizzle-orm", "postgres"], ["@electric-sql/pglite"]), ["drizzlePostgresDriver", "pgliteDriver", "postgresJsDriver"]);
  // Neon's Pool and Client are pg's shape, and its neon() function its own; pg-promise hands out pg's Pool.
  assert.deepEqual(of("postgres", ["@neondatabase/serverless"]), ["pgDriver", "neonDriver"]);
  assert.deepEqual(of("postgres", ["pg-promise"]), ["pgPromiseDriver", "pgDriver"]);
  assert.deepEqual(of("mysql", ["mysql2"]), ["mysql2Driver"]);
  assert.deepEqual(of("mysql", ["mariadb", "@planetscale/database", "knex"]), ["knexDriver", "mariadbDriver", "planetscaleDriver"]);
  // Effect's MySQL client runs on mysql2's connection.
  assert.deepEqual(of("mysql", ["effect", "@effect/sql-mysql2"]), ["mysql2Driver"]);
  // node:sqlite's way, a Durable Object's storage and D1 have no package, and every SQLite module passes them.
  assert.deepEqual(of("sqlite", ["better-sqlite3"]), ["durableObjectDriver", "syncSqliteDriver", "d1Driver"]);
  assert.deepEqual(of("sqlite", ["@libsql/client", "sqlite3"]), ["durableObjectDriver", "syncSqliteDriver", "libsqlDriver", "sqlite3Driver", "d1Driver"]);
  // An alias is the package it names, and its key the name the code imports.
  assert.deepEqual(installedDrivers("postgres", pkg({ dependencies: { pg8: "npm:pg@^8" } }))!.drivers, ["pgDriver"]);
  assert.deepEqual(installedDrivers("sqlite", pkg({ dependencies: { sqlite3: "npm:@vscode/sqlite3@^5" } }))!.drivers, ["durableObjectDriver", "syncSqliteDriver", "sqlite3Driver", "d1Driver"]);
  // A package that hands out a driver's client brings that driver.
  assert.deepEqual(of("sqlite", ["@astrojs/db"]), ["durableObjectDriver", "syncSqliteDriver", "libsqlDriver", "d1Driver"]);
  assert.deepEqual(of("postgres", ["@prisma/adapter-pg"]), ["pgDriver"]);
});

test("the lists are each run time's own, in its order, each name an export of reylo/module", () => {
  const named = (list: readonly unknown[]) => list.map((d) => Object.entries(reyloModule).find(([, v]) => v === d)?.[0]);
  assert.deepEqual(installedDrivers("postgres", null)!.drivers, named(postgresDrivers));
  assert.deepEqual(installedDrivers("mysql", null)!.drivers, named(mysqlDrivers));
  assert.deepEqual(installedDrivers("sqlite", null)!.drivers, named(sqliteDrivers));
});

test("Bun's drivers by its lockfile, its types or a packageManager naming it", () => {
  assert.deepEqual(of("postgres", ["pg"], [], { "bun.lock": "" }), ["pgDriver", "bunPostgresDriver"]);
  assert.deepEqual(of("postgres", ["pg"], ["@types/bun"]), ["pgDriver", "bunPostgresDriver"]);
  assert.deepEqual(installedDrivers("mysql", pkg({ packageManager: "bun@1.3.0", dependencies: { mysql2: "*" } }))!.drivers, ["bunMysqlDriver", "mysql2Driver"]);
});

test("an app that declares no driver's own package as a runtime dependency says nothing of its driver, and its modules pass every one; so does a module no bundler builds, and a build rooted at a workspace's root", () => {
  assert.equal(of("postgres", ["left-pad"]).length, 10);
  // A driver only its tests use.
  assert.equal(of("postgres", [], ["@electric-sql/pglite"]).length, 10);
  assert.equal(installedDrivers("postgres", pkg({ dependencies: { pg: "*" }, workspaces: ["apps/*"] }))!.drivers.length, 10);
  // A wrapper and a runtime name no driver.
  assert.equal(of("postgres", ["drizzle-orm", "knex", "effect"], [], { "bun.lock": "" }).length, 10);
  assert.equal(of("sqlite", ["wrangler"]).length, 12);
  assert.equal(installedDrivers("postgres", mkdtempSync(join(tmpdir(), "reylo-drivers-")))!.drivers.length, 10);
  assert.equal(installedDrivers("postgres", null)!.drivers.length, 10);
});

test("a workspace package the app depends on brings its dependencies, its devDependencies too, as does the workspace's root", () => {
  const top = pkg({ name: "top", dependencies: { "@neondatabase/serverless": "*" } }, { "pnpm-workspace.yaml": "packages:\n- apps/*\n# the shared code\n- 'packages/**'\n" });
  // The queries' package runs its tests on PGlite, and depends on postgres for its seed script.
  pkg({ name: "@acme/db", dependencies: { postgres: "*" }, devDependencies: { "@electric-sql/pglite": "*" } }, {}, join(top, "packages", "data", "db"));
  const web = pkg({ name: "web", dependencies: { "@acme/db": "workspace:*", pg: "*" } }, {}, join(top, "apps", "web"));
  const found = installedDrivers("postgres", web)!;
  assert.deepEqual(found.drivers, ["pgliteDriver", "pgDriver", "neonDriver", "postgresJsDriver"]);
  assert.ok(found.watched.includes(join(top, "packages", "data", "db", "package.json")));
  assert.ok(found.watched.includes(join(top, "pnpm-workspace.yaml")));
  // A flow list, and package.json's workspaces with braces.
  const flow = pkg({ name: "top" }, { "pnpm-workspace.yaml": "packages: [apps/*, libs/*]\n" });
  pkg({ name: "lib", dependencies: { mysql2: "*" } }, {}, join(flow, "libs", "lib"));
  assert.deepEqual(installedDrivers("mysql", pkg({ name: "app", dependencies: { lib: "workspace:^" } }, {}, join(flow, "apps", "app")))!.drivers, ["mysql2Driver"]);
  const npm = pkg({ workspaces: ["{apps,libs}/*"] });
  pkg({ name: "lib", dependencies: { mariadb: "*" } }, {}, join(npm, "libs", "lib"));
  assert.deepEqual(installedDrivers("mysql", pkg({ name: "app", dependencies: { lib: "*" } }, {}, join(npm, "apps", "app")))!.drivers, ["mariadbDriver"]);
});

test("a package reached through file:, workspace: with a path, and the .sql file's own package, bring their dependencies", () => {
  // pnpm links a relative workspace: spec to the package at that path, under the alias.
  const top = pkg({ name: "top" }, { "pnpm-workspace.yaml": "packages:\n  - apps/*\n  - libs/*\n" });
  pkg({ name: "@acme/db", dependencies: { "@libsql/client": "*" } }, {}, join(top, "libs", "db"));
  const app = pkg({ name: "app", dependencies: { database: "workspace:../../libs/db", sqlite3: "*" } }, {}, join(top, "apps", "app"));
  assert.deepEqual(installedDrivers("sqlite", app)!.drivers, ["durableObjectDriver", "syncSqliteDriver", "libsqlDriver", "sqlite3Driver", "d1Driver"]);
  const shared = pkg({ name: "shared", dependencies: { postgres: "*" } });
  assert.deepEqual(installedDrivers("postgres", pkg({ dependencies: { shared: `file:${shared}` } }))!.drivers, ["postgresJsDriver"]);
  // An app that reaches the queries' package by a path, with no dependency on it.
  const queries = pkg({ name: "queries", dependencies: { pg: "*" } });
  assert.deepEqual(installedDrivers("postgres", pkg({ dependencies: { postgres: "*" } }), join(queries, "sql"))!.drivers, ["pgDriver", "postgresJsDriver"]);
});
