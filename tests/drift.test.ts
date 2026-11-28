// A module whose statement's result no longer has its columns, after a migration under `select *`:
// each column it declares is still read by its kind, a type the database created among them, with
// no catalog lookup in the process (src/decode.ts, `declaredColumn`), and a column it doesn't declare
// is the server's text. This file defines no query by hand, so nothing installs the lookup.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { sqlModule } from "../src/loader.ts";
import { closeSessions, driver, MODULE, pgLive, postgresDatabase } from "./helpers.ts";

test("postgres: after a migration adds a column under select *, a created type the module declares is still read by its kind on pg, postgres.js and PGlite", { skip: pgLive, timeout: 60_000 }, async () => {
  const own = await postgresDatabase("reylo_drift", "create type mood as enum ('happy', 'sad'); create table items (id int primary key, tags mood[] not null); insert into items values (1, '{happy,sad}')");
  const root = mkdtempSync(join(tmpdir(), "reylo-drift-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, ".env"), `DATABASE_URL=${own.url}\n`);
  const sql = "select * from items order by id";
  writeFileSync(join(root, "items.sql"), sql);
  const { code } = await sqlModule(join(root, "items.sql"), sql);
  writeFileSync(join(root, "items.mjs"), code.replace(`from "reylo/module"`, `from ${JSON.stringify(MODULE)}`));
  const items = (await import(pathToFileURL(join(root, "items.mjs")).href)).default;
  const postgres = driver("postgres");
  const admin = postgres(own.url, { max: 1, onnotice: () => {} });
  await admin.unsafe("alter table items add column note text; update items set note = 'x'");
  const pg = driver("pg");
  const client = new pg.Client({ connectionString: own.url });
  await client.connect();
  const js = postgres(own.url, { max: 1 });
  try {
    for (const db of [client, js]) assert.deepEqual(await items.run(db), [{ id: 1, tags: ["happy", "sad"], note: "x" }]);
  } finally {
    await client.end();
    await js.end();
    await admin.end();
    await closeSessions();
    await own.drop();
  }
});
