import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { canonical, normalized, sameMigration, transactionControl, turnsForeignKeysOn } from "../src/migration-text.ts";

// The cases engine/migrate/src/lock.rs's and commands.rs's tests read too, so that the port reads every one as the Rust does.
type Cases = { canonical: { sql: string; canonical: string }[]; same: { a: string; b: string }[]; differ: { a: string; b: string }[]; normalized: { sql: string; normalized: string }[] };
const cases: Cases = JSON.parse(readFileSync(new URL("../engine/migrate/tests/migration-text-cases.json", import.meta.url), "utf8"));

test("comments and spacing are no edit, as the lock reads them", () => {
  for (const c of cases.canonical) assert.equal(canonical(c.sql), c.canonical, JSON.stringify(c));
  for (const c of cases.same) assert.equal(canonical(c.a), canonical(c.b), JSON.stringify(c));
  for (const c of cases.differ) assert.notEqual(canonical(c.a), canonical(c.b), JSON.stringify(c));
});

test("line endings and trailing spacing read as the shared cases say, and a statement's change is an edit", () => {
  for (const c of cases.normalized) assert.equal(normalized(c.sql), c.normalized, JSON.stringify(c));
  assert.ok(sameMigration("create table t (\r\n  a int -- the key\r\n);\r\n", "create table t (\n  a int\n);"));
  assert.ok(sameMigration("insert into t values ('a\r\nb');", "insert into t values ('a\nb');"));
  assert.ok(!sameMigration("create table t (a int);", "create table t (a bigint);"));
});

// SQLite's statements that control the transaction, from the corpus `atomicity_problem` in
// engine/migrate/src/sql.rs is held to too, so that the two readers can't drift apart.
test("a SQLite migration's own begin, commit, end, rollback, savepoint or release is found as the command line finds it", () => {
  const corpus = JSON.parse(readFileSync(new URL("../engine/migrate/tests/atomicity.json", import.meta.url), "utf8")) as { cases: { sql: string; control: string | null }[] };
  for (const { sql, control } of corpus.cases) assert.equal(transactionControl(sql) ?? null, control, sql);
});

test("a migration that turns foreign keys on is found however SQLite reads the pragma", () => {
  for (const sql of ["pragma foreign_keys = on;", "PRAGMA \"foreign_keys\" = ON;", "pragma main.foreign_keys = 2;", "pragma foreign_keys = 01;", "pragma foreign_keys = 0x1;", "pragma foreign_keys = 1.0;", "pragma foreign_keys('yes');", "pragma foreign_keys = full;", "create table a (b); pragma [Foreign_Keys] = true;", "explain pragma foreign_keys = on;", "EXPLAIN QUERY PLAN PRAGMA foreign_keys = 1;", "explain query plan format = json pragma foreign_keys = on;"]) assert.ok(turnsForeignKeysOn(sql), sql);
  for (const sql of ["pragma foreign_keys = off;", "pragma foreign_keys = 0;", "PRAGMA foreign_keys = 'No';", "pragma foreign_keys;", "pragma foreign_key_check;", "-- pragma foreign_keys = on\ncreate table a (b);", "select 'pragma foreign_keys = on';", "create table pragma (foreign_keys integer);", "create table t (pragma text, foreign_keys int, yes int);"]) assert.ok(!turnsForeignKeysOn(sql), sql);
});
