import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { nativeBinary } from "../src/native.ts";
import { embeddedModule, lockChecksum, projectMigrations, readLock } from "../src/embedded-module.ts";

// What Reylo's bundler plugins put in place of reylo's placeholder module: the migrations `reylo
// migrate deploy` runs, in its order, which each Durable Object holds its history against when it
// starts (tests/migrate.test.ts).
// The project is made with reylo migrate itself, on SQLite, so no server is needed.

/** `reylo migrate <args>` in `root`, outside CI, where `new` and `dev` refuse to run. */
function migrate(root: string, ...args: string[]) {
  const env = { ...process.env };
  delete env.CI;
  const r = spawnSync(nativeBinary(), ["migrate", ...args], { cwd: root, encoding: "utf8", env });
  assert.equal(r.status, 0, r.stdout + r.stderr);
}

function project() {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "reylo-project-module-")));
  writeFileSync(join(root, "package.json"), `{ "type": "module" }`);
  writeFileSync(join(root, ".reylo.yaml"), "direct_url: file:./dev.sqlite\n");
  mkdirSync(join(root, "reylo"));
  return root;
}

/** The migrations the bundle carries, by name. */
async function names(root: string) {
  const { list, error } = await projectMigrations(root);
  assert.equal(error, undefined);
  return list!.map((m) => m.name);
}

/** Writes schema.sql, then the migration it needs, a second later so names sort in the order written. */
async function change(root: string, name: string, schema: string, run = true) {
  await new Promise((r) => setTimeout(r, 1100));
  writeFileSync(join(root, "reylo", "schema.sql"), schema);
  migrate(root, "new", name);
  if (run) migrate(root, "dev");
}

test("the bundle carries the locked migrations in the lock's order, and a draft gives migrate the deploy's refusal", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  migrate(root, "lock");
  await change(root, "bio", "create table users (id integer primary key, name text not null, bio text);\n");
  const [users, bio] = readdirSync(join(root, "reylo", "migrations")).sort();
  assert.deepEqual(await names(root), [users, bio]);
  const { code, watched } = await embeddedModule(root);
  assert.match(code, /^export const embedded = \[\{"name":/);
  assert.ok(code.includes("alter table users add column bio text"));
  // Nothing of the project file itself, whose direct_url can hold a password.
  assert.ok(!code.includes("dev.sqlite"));
  assert.ok(watched.includes(join(root, "reylo", "reylo.lock")));
  // A draft, which the lock doesn't list yet, is refused, as a deploy refuses it.
  await change(root, "age", "create table users (id integer primary key, name text not null, bio text, age int);\n", false);
  const [, , age] = readdirSync(join(root, "reylo", "migrations")).sort();
  assert.equal((await projectMigrations(root)).error, `The lock doesn't list reylo/migrations/${age} yet: run it in development, which locks it, with reylo migrate dev`);
  migrate(root, "dev");
  assert.deepEqual(await names(root), [users, bio, age]);
});

test("a file that isn't a migration, or a schema.sql the lock's checksum isn't of, gives migrate the deploy's refusal", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  migrate(root, "lock");
  const [users] = readdirSync(join(root, "reylo", "migrations"));
  const schema = join(root, "reylo", "schema.sql");
  const text = readFileSync(schema, "utf8");
  // A comment or spacing is no change, as the lock reads schema.sql.
  writeFileSync(schema, `-- the users\n${text.replace(/ /g, "  ")}`);
  assert.deepEqual(await names(root), [users]);
  writeFileSync(schema, text.replace("name text not null", "name text not null, bio text"));
  assert.match((await projectMigrations(root)).error ?? "", /^The schema changed since the lock was written, in reylo\/schema\.sql: if the schema changed, write its migration and run it/);
  writeFileSync(schema, text);
  writeFileSync(join(root, "reylo", "migrations", "notes.sql"), "select 1;\n");
  const stray = "Not a migration: notes.sql, in reylo/migrations, where a file is init.sql or <YYYYMMDDHHMMSS>_<name>.sql, the name in a-z, 0-9 and _";
  assert.equal((await projectMigrations(root)).error, stray);
  // With no lock, a stray is refused too, never run as a migration.
  rmSync(join(root, "reylo", "reylo.lock"));
  assert.equal((await projectMigrations(root)).error, stray);
  rmSync(join(root, "reylo", "migrations", "notes.sql"));
  assert.deepEqual(await names(root), [users]);
});

test("a lock with git's conflict markers, or one that lists a file the folder lacks, gives migrate the error to throw", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  migrate(root, "lock");
  const [users] = readdirSync(join(root, "reylo", "migrations"));
  const lock = join(root, "reylo", "reylo.lock");
  const text = readFileSync(lock, "utf8");
  writeFileSync(lock, text.replace(/^migrations:\n/m, "migrations:\n<<<<<<< HEAD\n  - name: 20261110000000_a.sql\n    checksum: 0123abcd\n=======\n  - name: 20261110000000_b.sql\n    checksum: 4567cdef\n>>>>>>> branch\n"));
  assert.match((await projectMigrations(root)).error ?? "", /^The lock has a merge conflict, in reylo\/reylo\.lock: keep main's lines, then run reylo migrate dev/);
  assert.match((await embeddedModule(root)).code, /^export const embedded = \{"error":"The lock has a merge conflict, in reylo\/reylo\.lock/);
  writeFileSync(lock, text);
  renameSync(join(root, "reylo", "migrations", users!), join(root, "aside.sql"));
  assert.match((await projectMigrations(root)).error ?? "", new RegExp(`^The lock lists ${users!.replace(".", "\\.")}, which reylo/migrations doesn't have: write it back from a database that ran it, with reylo migrate history pull`));
});

test("a migration edited after it was locked, or lines merged out of order, give migrate the deploy's refusal", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  await change(root, "bio", "create table users (id integer primary key, name text not null, bio text);\n");
  migrate(root, "lock");
  const [users, bio] = readdirSync(join(root, "reylo", "migrations")).sort();
  const file = join(root, "reylo", "migrations", bio!);
  const text = readFileSync(file, "utf8");
  // A comment, spacing or CRLF line endings are no edit, as the lock reads them.
  writeFileSync(file, `-- the profile's text\r\n${text.replace(/\n/g, "\r\n").replace(/ /g, "  ")}\r\n`);
  assert.deepEqual(await names(root), [users, bio]);
  writeFileSync(file, text.replace("bio text", "bio text not null default ''"));
  assert.match((await projectMigrations(root)).error ?? "", new RegExp(`^Migration reylo/migrations/${bio!.replace(".", "\\.")} changed after it was locked: a database that ran the old text refuses it`));
  writeFileSync(file, text);
  // Two branches that each locked a migration after users, merged as "accept both": the second
  // line is chained after users, not after the line before it.
  const lock = join(root, "reylo", "reylo.lock");
  const lines = readFileSync(lock, "utf8");
  const sum = /^    checksum: '?([0-9a-f]+)'?$/m.exec(lines)![1]!;
  const age = "20990101000000_age.sql";
  writeFileSync(join(root, "reylo", "migrations", age), "alter table users add column age int;\n");
  writeFileSync(lock, lines.replace(/^  - name: .*\n    checksum: .*\n(?![\s\S]*^  - )/m, (bioEntry) => `  - name: ${age}\n    checksum: '${lockChecksum(sum, age, "alter table users add column age int;")}'\n${bioEntry}`));
  assert.match((await projectMigrations(root)).error ?? "", new RegExp(`^The lock was merged out of order at ${bio!.replace(".", "\\.")}`));
});

test("a lock written up to 0.8.2, each entry one line, gives the same migrations", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  await change(root, "bio", "create table users (id integer primary key, name text not null, bio text);\n");
  migrate(root, "lock");
  const lock = join(root, "reylo", "reylo.lock");
  const text = readFileSync(lock, "utf8");
  assert.match(text, /^migrations:\n  - name: \d{14}_users\.sql\n    checksum: '?[0-9a-f]{8}'?\n  - name: \d{14}_bio\.sql\n/m);
  const before = await projectMigrations(root);
  writeFileSync(lock, text.replace(/^  - name: (\S+)\n    checksum: '?([0-9a-f]+)'?$/gm, "  - $1 $2"));
  assert.match(readFileSync(lock, "utf8"), /^migrations:\n  - \d{14}_users\.sql [0-9a-f]{8}\n  - \d{14}_bio\.sql [0-9a-f]{8}\n$/m);
  assert.deepEqual(await projectMigrations(root), before);
  assert.equal(before.list?.length, 2);
});

test("the lock reads as the command line reads it: keys in either order, quoted values, entries written up to 0.8.2", () => {
  const lock = { schema: "c7bf681b", entries: [{ name: "20261104120000_users.sql", sum: "c86e9de4" }, { name: "20261104140347_add_bio.sql", sum: "00123456" }] };
  const objects = "# On a conflict, keep main's and run reylo migrate dev\nschema: c7bf681b\nmigrations:\n  - name: 20261104120000_users.sql\n    checksum: c86e9de4\n  - name: 20261104140347_add_bio.sql\n    checksum: '00123456'\n";
  assert.deepEqual(readLock(objects), lock);
  assert.deepEqual(readLock(objects.replace(/\n/g, "\r\n")), lock);
  assert.deepEqual(readLock(objects.replace("schema: c7bf681b", 'schema: "c7bf681b"')), lock);
  assert.deepEqual(readLock(objects.replace("  - name: 20261104120000_users.sql\n    checksum: c86e9de4\n", "  - checksum: c86e9de4\n    name: 20261104120000_users.sql\n")), lock);
  assert.deepEqual(readLock(objects.replace("  - name: 20261104120000_users.sql\n    checksum: c86e9de4\n", "  - { name: 20261104120000_users.sql, checksum: c86e9de4 }\n")), lock);
  assert.deepEqual(readLock("schema: c7bf681b\nmigrations:\n  - 20261104120000_users.sql c86e9de4\n  - 20261104140347_add_bio.sql 00123456\n"), lock);
  assert.deepEqual(readLock("migrations:\n"), { entries: [] });
  assert.deepEqual(readLock(`version: 1\n${objects}`), lock);
  for (const text of ["version: '1'\nmigrations:\n", "version:\nmigrations:\n", "version: NULL\nmigrations:\n", "version: ~ # none\nmigrations:\n", "migrations:\n"]) assert.deepEqual(readLock(text), { entries: [] }, text);
  // A list written without indentation, as YAML allows.
  assert.deepEqual(readLock(objects.replace(/^  (- |  )/gm, "$1")), lock);
  assert.deepEqual(readLock("schema: c7bf681b\nmigrations:\n- 20261104120000_users.sql c86e9de4\n- 20261104140347_add_bio.sql 00123456\n"), lock);
});

test("the lock refuses what the command line refuses: a key it doesn't know, a key twice, an entry without its name or checksum", () => {
  for (const [text, why] of [
    ["migrations:\n  - name: a.sql\n    checksum: c86e9de4\n    transaction: false\n", "its line 4 has `transaction`, which an entry doesn't have"],
    ["migrations:\n  - name: a.sql\n    checksum: 00000000\n    checksum: c86e9de4\n", "its line 4 gives `checksum` a second time"],
    ["migrations:\n  - name: a.sql\n", "the entry at its line 2 has no checksum"],
    ["migrations:\n  - checksum: c86e9de4\n", "the entry at its line 2 has no name"],
    ["migrations:\n  - a.sql\n", '"a.sql" is a migration\'s name and its checksum'],
    ["schema: c7bf681b\nschema:\nmigrations:\n", "its line 2 gives `schema` a second time"],
    ...["0", "01", "+1", "0x2", "1.0", "one"].map((v) => [`version: ${v}\nmigrations:\n`, `\`version: ${v}\` isn't a lock's version`] as const),
    ["version: 'null'\nmigrations:\n", "`version: null` isn't a lock's version"],
    ["migrations:\n  - a.sql c86e9de4\n- b.sql 78ad08c0\n", "its line 3 doesn't parse"],
    ["migrations:\n- name: a.sql\n  checksum: c86e9de4\n  - name: b.sql\n    checksum: 78ad08c0\n", "its line 4 doesn't parse"],
  ] as const) {
    assert.deepEqual(readLock(text), { error: why }, text);
  }
});

test("a lock a newer Reylo wrote is refused by its version, whatever the rest holds", () => {
  assert.deepEqual(readLock("version: 2\nmigrations:\n  - name: a.sql\n    checksum: c86e9de4\n    transaction: false\n"), { newer: 2 });
  assert.deepEqual(readLock('version: "2" # upgraded\nmigrations:\n'), { newer: 2 });
});

test("a lock the command line refuses gives migrate its error, and a quoted schema checksum is still held to schema.sql", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  migrate(root, "lock");
  const lock = join(root, "reylo", "reylo.lock");
  const text = readFileSync(lock, "utf8");
  // A name that isn't a migration file, a directory's included.
  writeFileSync(lock, text.replace(/^  - name: \S+$/m, "  - name: ."));
  assert.match((await projectMigrations(root)).error ?? "", /^The lock lists \., which reylo\/migrations doesn't have/);
  assert.match(text, /^# On a conflict, keep main's and run reylo migrate dev\nversion: 1\nschema: /);
  writeFileSync(lock, text.replace(/^version: 1$/m, "version: 2"));
  assert.match((await projectMigrations(root)).error ?? "", /^A newer Reylo wrote reylo\/reylo\.lock: it is a lock of version 2, and this reylo reads version 1 and older, so upgrade reylo to read it$/);
  writeFileSync(lock, text.replace(/^(    checksum: .*)$/m, "$1\n    transaction: false"));
  assert.match((await projectMigrations(root)).error ?? "", /^Reylo can't read reylo\/reylo\.lock as a lock, since its line \d+ has `transaction`, which an entry doesn't have: write it again from a database that ran its migrations, with reylo migrate history pull$/);
  assert.match(text, /^schema: '?[0-9a-f]{8}'?$/m);
  writeFileSync(lock, text.replace(/^schema: '?([0-9a-f]+)'?$/m, 'schema: "$1"'));
  assert.equal((await projectMigrations(root)).error, undefined);
  writeFileSync(join(root, "reylo", "schema.sql"), "create table users (id integer primary key, name text);\n");
  assert.match((await projectMigrations(root)).error ?? "", /^The schema changed since the lock was written/);
});

test("with no lock, the migrations run in name order, init.sql first", async () => {
  const root = project();
  mkdirSync(join(root, "reylo", "migrations"));
  writeFileSync(join(root, "reylo", "schema.sql"), "create table users (id integer primary key, name text not null, bio text);\n");
  writeFileSync(join(root, "reylo", "migrations", "20261106120000_bio.sql"), "alter table users add column bio text;\n");
  writeFileSync(join(root, "reylo", "migrations", "init.sql"), "create table users (id integer primary key, name text not null);\n");
  assert.deepEqual(await names(root), ["init.sql", "20261106120000_bio.sql"]);
});

test("the lock under its old name, alone or beside the lock, gives migrate the error to throw, never the migrations in name order", async () => {
  const root = project();
  await change(root, "users", "create table users (id integer primary key, name text not null);\n");
  migrate(root, "lock");
  const lock = join(root, "reylo", "reylo.lock");
  const old = join(root, "reylo", "reylo-lock.yaml");
  const text = readFileSync(lock, "utf8");
  renameSync(lock, old);
  assert.equal((await projectMigrations(root)).error, "The lock's old name is reylo/reylo-lock.yaml: rename it to reylo/reylo.lock, with git mv reylo/reylo-lock.yaml reylo/reylo.lock");
  assert.match((await embeddedModule(root)).code, /^export const embedded = \{"error":"The lock's old name is reylo\/reylo-lock\.yaml/);
  assert.ok((await embeddedModule(root)).watched.includes(old));
  writeFileSync(lock, text);
  assert.match((await projectMigrations(root)).error ?? "", /^Both reylo\/reylo-lock\.yaml and reylo\/reylo\.lock are here: .* with git rm reylo\/reylo-lock\.yaml$/);
  rmSync(old);
  assert.equal((await projectMigrations(root)).error, undefined);
});

test("a project with no migrations gives the bundle none, and builds as before", async () => {
  const root = project();
  assert.equal((await embeddedModule(root)).code, "export const embedded = undefined;\n");
  rmSync(root, { recursive: true, force: true });
});
