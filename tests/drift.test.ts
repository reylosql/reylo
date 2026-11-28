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
  const built = async (name: string, sql: string) => {
    writeFileSync(join(root, `${name}.sql`), sql);
    const { code } = await sqlModule(join(root, `${name}.sql`), sql);
    writeFileSync(join(root, `${name}.mjs`), code.replace(`from "reylo/module"`, `from ${JSON.stringify(MODULE)}`));
    return (await import(pathToFileURL(join(root, `${name}.mjs`)).href)).default;
  };
  const items = await built("items", "select * from items order by id");
  // A column the migration adds under a name a later column of the statement holds: only the field the row keeps is matched by name.
  const shadowed = await built("shadowed", "select items.id, array['happy'::mood] as moods, 'ok'::text as label from items");
  const postgres = driver("postgres");
  const admin = postgres(own.url, { max: 1, onnotice: () => {} });
  await admin.unsafe("alter table items add column note text; update items set note = 'x'");
  const shadowedNow = await built("shadowed_now", "select items.*, array['happy'::mood] as moods, 'ok'::text as tags from items");
  await admin.unsafe("alter table items add column extra int");
  const pg = driver("pg");
  const client = new pg.Client({ connectionString: own.url });
  await client.connect();
  const js = postgres(own.url, { max: 1 });
  try {
    for (const db of [client, js]) assert.deepEqual(await items.run(db), [{ id: 1, tags: ["happy", "sad"], note: "x", extra: null }]);
    void shadowed;
    // The statement's own `tags` is text; the table's, which a migration added, is an enum array the
    // row doesn't keep. On clients that have met neither type, so that nothing learned before decides.
    const fresh = new pg.Client({ connectionString: own.url });
    await fresh.connect();
    const freshJs = postgres(own.url, { max: 1 });
    try {
      for (const db of [fresh, freshJs]) assert.deepEqual(await shadowedNow.run(db), [{ id: 1, tags: "ok", note: "x", extra: null, moods: ["happy"] }]);
    } finally {
      await fresh.end();
      await freshJs.end();
    }
    // postgres.js renaming every column: the module's names are matched under its renaming.
    const upper = postgres(own.url, { max: 1, transform: { column: { from: (name: string) => name.toUpperCase() } } });
    try {
      assert.deepEqual(await items.run(upper), [{ ID: 1, TAGS: ["happy", "sad"], NOTE: "x", EXTRA: null }]);
      // A column a migration adds under a name the renaming gives a declared one: neither takes the declared kind.
    await admin.unsafe(`create table lists (id int primary key, mood_list mood[]); insert into lists values (1, '{sad}')`);
    const lists = await built("lists", "select * from lists");
    await admin.unsafe(`alter table lists add column "moodList" mood; update lists set "moodList" = 'sad'`);
    const camel = postgres(own.url, { max: 1, transform: postgres.camel });
    try {
      assert.deepEqual(await lists.run(camel), [{ id: 1, moodList: "sad" }]);
    } finally {
      await camel.end();
    }
    // A transaction keeps no options: its names match without their case.
      assert.deepEqual(await upper.begin((tx: unknown) => items.run(tx)), [{ ID: 1, TAGS: ["happy", "sad"], NOTE: "x", EXTRA: null }]);
    } finally {
      await upper.end();
    }
  } finally {
    await client.end();
    await js.end();
    await admin.end();
    await closeSessions();
    await own.drop();
  }
});
