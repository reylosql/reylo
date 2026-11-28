import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { canonical, normalized, sameMigration } from "../src/migration-text.ts";

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
