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
  // A plain enum, which a later query on a client reads after a drifted one.
  const mood = await built("mood", "select 'sad'::mood as m");
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
    } finally {
      await upper.end();
    }
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
    // The declared column retyped, and a column added under the name the renaming gives it: the one
    // field of that name is not the declaration's, and what it is read as teaches the client nothing.
    await admin.unsafe(`create table retyped (id int primary key, mood_list mood[]); insert into retyped values (1, '{sad}')`);
    const retyped = await built("retyped", "select * from retyped");
    await admin.unsafe(`alter table retyped drop column mood_list; alter table retyped add column mood_list int; alter table retyped add column "moodList" mood; update retyped set mood_list = 1, "moodList" = 'sad'`);
    const camelRetyped = postgres(own.url, { max: 1, transform: postgres.camel });
    try {
      assert.deepEqual(await retyped.run(camelRetyped), [{ id: 1, moodList: "sad" }]);
      assert.deepEqual(await mood.run(camelRetyped), { m: "sad" });
    } finally {
      await camelRetyped.end();
    }
    // The statement's alias still wins in the row, beside a column a migration adds under its name:
    // nothing says which field the declaration is, so the alias is read as its text, not by a guess.
    await admin.unsafe(`create table tagged (id int primary key); insert into tagged values (1)`);
    const aliased = await built("aliased", "select tagged.*, array['sad'::mood] as tags from tagged");
    await admin.unsafe(`alter table tagged add column tags int`);
    const freshAlias = postgres(own.url, { max: 1 });
    const freshAliasPg = new pg.Client({ connectionString: own.url });
    await freshAliasPg.connect();
    try {
      for (const db of [freshAlias, freshAliasPg]) assert.deepEqual(await aliased.run(db), [{ id: 1, tags: "{sad}" }]);
    } finally {
      await freshAlias.end();
      await freshAliasPg.end();
    }
    // An array of two dimensions, whose oid is its element's array's: it fits its declaration, so it and
    // an added enum of its name are both left, and neither takes the declared kind, nor teaches the
    // client one for the enum.
    await admin.unsafe(`create table things (id int primary key); insert into things values (1)`);
    const ranked = await built("ranked", "select array[[1,2],[3,4]] as tags, things.* from things");
    await admin.unsafe(`alter table things add column tags mood; update things set tags = 'sad'`);
    const freshRank = postgres(own.url, { max: 1 });
    try {
      assert.deepEqual(await ranked.run(freshRank), [{ tags: "sad", id: 1 }]);
      assert.deepEqual(await mood.run(freshRank), { m: "sad" });
    } finally {
      await freshRank.end();
    }
    // A row value's column retyped to a built-in type: it is read by its type, not as the row it was.
    await admin.unsafe(`create type pair as (n int, s text); create table payloads (id int primary key, payload pair); insert into payloads values (1, row(1, 'a'))`);
    const payloads = await built("payloads", "select * from payloads");
    await admin.unsafe(`alter table payloads drop column payload; alter table payloads add column payload int; alter table payloads add column note text; update payloads set payload = 42`);
    const freshPayload = postgres(own.url, { max: 1 });
    try {
      assert.deepEqual(await payloads.run(freshPayload), [{ id: 1, payload: 42, note: null }]);
    } finally {
      await freshPayload.end();
    }
    // A column renamed and another added under a name like it: the new one is no declared column.
    await admin.unsafe(`create table moods (id int primary key, mood_list mood[]); insert into moods values (1, '{sad}')`);
    const moods = await built("moods", "select * from moods");
    await admin.unsafe(`alter table moods rename column mood_list to retired_moods; alter table moods add column moodlist mood; update moods set moodlist = 'sad'`);
    const renamed = new pg.Client({ connectionString: own.url });
    await renamed.connect();
    try {
      assert.deepEqual(await moods.run(renamed), [{ id: 1, retired_moods: "{sad}", moodlist: "sad" }]);
    } finally {
      await renamed.end();
    }
  } finally {
    await client.end();
    await js.end();
    await admin.end();
    await closeSessions();
    await own.drop();
  }
});
