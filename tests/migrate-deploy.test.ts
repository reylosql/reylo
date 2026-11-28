import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { nativeBinary } from "../src/native.ts";
import { migrate, type MigrationConnection } from "../src/migrate.ts";
import { driver, ROOT } from "./helpers.ts";

// `await migrate()` on a server runs `reylo migrate deploy` in the working directory, as the command
// line does there, on SQLite so that no server is needed.
test("await migrate() runs reylo migrate deploy, resolves to what it ran, and rejects with the deploy's problems", async () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "reylo-migrate-deploy-")));
  writeFileSync(join(root, "package.json"), `{ "type": "module" }`);
  writeFileSync(join(root, ".reylo.yaml"), "direct_url: file:./dev.sqlite\n");
  mkdirSync(join(root, "reylo"));
  mkdirSync(join(root, "node_modules"));
  symlinkSync(ROOT, join(root, "node_modules", "reylo"));
  writeFileSync(join(root, "reylo", "schema.sql"), "create table users (id integer primary key, name text not null);\n");
  const env = { ...process.env };
  delete env.CI;
  for (const args of [["new", "users"], ["dev"], ["lock"]]) {
    const r = spawnSync(nativeBinary(), ["migrate", ...args], { cwd: root, encoding: "utf8", env });
    assert.equal(r.status, 0, r.stdout + r.stderr);
  }
  const was = process.cwd();
  process.chdir(root);
  try {
    const ran = await migrate({ url: "file:./prod.sqlite" });
    assert.equal(ran.length, 1);
    assert.match(ran[0]!, /^\d{14}_users\.sql$/);
    assert.deepEqual(await migrate({ url: "file:./prod.sqlite" }), []);
    // The project's own database, which the dev run took to the migrations already.
    assert.deepEqual(await migrate(), []);
    // On the app's own connection, as Turso's engine holds its file: the project's locked migrations, read here.
    const turso: MigrationConnection & { close(): Promise<void> } = await driver("@tursodatabase/database").connect(join(root, "turso.db"));
    assert.deepEqual(await migrate(turso), ran);
    assert.deepEqual(await migrate(turso), []);
    // A draft refuses the deploy, with what to do about it.
    writeFileSync(join(root, "reylo", "migrations", "20991231000000_draft.sql"), "alter table users add column bio text;\n");
    await assert.rejects(migrate({ url: "file:./prod.sqlite" }), /^Error: reylo migrate deploy refused: The lock doesn't list 20991231000000_draft\.sql yet\n  Run it in development, which locks it: reylo migrate dev/);
    await assert.rejects(migrate(turso), /^Error: reylo: The lock doesn't list reylo\/migrations\/20991231000000_draft\.sql yet: run it in development, which locks it, with reylo migrate dev$/);
    await turso.close();
    rmSync(join(root, "reylo", "migrations", "20991231000000_draft.sql"));
    // A database changed by hand refuses it, naming the change as the command line does.
    const changed = new DatabaseSync(join(root, "prod.sqlite"));
    changed.exec("alter table users add column hotfix integer");
    await assert.rejects(migrate({ url: "file:./prod.sqlite" }), /^Error: reylo migrate deploy refused: This database was changed outside its migrations\n  \+ column users\.hotfix integer\n  If a migration makes this change/);
    changed.exec("alter table users drop column hotfix");
    changed.close();
    // A migration that fails on this database rolls back, and rejects with what to run next.
    writeFileSync(join(root, "reylo", "schema.sql"), "create table users (id integer primary key, name text not null);\ncreate unique index users_name on users (name);\n");
    for (const args of [["new", "unique_name"], ["dev"]]) {
      const r = spawnSync(nativeBinary(), ["migrate", ...args], { cwd: root, encoding: "utf8", env });
      assert.equal(r.status, 0, r.stdout + r.stderr);
    }
    const prod = new DatabaseSync(join(root, "prod.sqlite"));
    prod.exec("insert into users (name) values ('ada'), ('ada')");
    prod.close();
    await assert.rejects(migrate({ url: "file:./prod.sqlite" }), /^Error: reylo migrate deploy failed: The migration \d{14}_unique_name\.sql failed and was rolled back\n  UNIQUE constraint failed: users\.name\n  Nothing was recorded, so the next deploy runs it again\.\n  Fix the file, lock the fix and deploy again: reylo migrate dev, then reylo migrate deploy\n  If another database already ran it, write a new migration instead: reylo migrate new <name>$/);
  } finally {
    process.chdir(was);
  }
});
