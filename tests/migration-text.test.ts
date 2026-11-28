import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { canonical, normalized, sameMigration, transactionControl, turnsForeignKeysOn } from "../src/migration-text.ts";

// The port of `canonical` in engine/migrate/src/lock.rs: the Rust tests' cases, each read the same.
test("comments and spacing are no edit, as the lock reads them", () => {
  const a = "create table a (\n  id int -- the key\n);\n/* made by hand */";
  assert.equal(canonical(a), canonical("create table a ( id int );"));
  assert.notEqual(canonical("insert into a values ('-- not a comment')"), canonical("insert into a values ('')"));
  assert.equal(canonical("select $$ two  spaces $$"), "select $$ two  spaces $$");
  assert.equal(canonical("do $f$ begin -- x\n end $f$;"), "do $f$ begin -- x\n end $f$;");
  // MySQL's `\'` is a quote inside the string, so `-- b` is string text there, not a comment.
  assert.notEqual(canonical("insert into t values ('a\\' -- b')"), canonical("insert into t values ('a\\' -- c')"));
  assert.notEqual(canonical("select E'a\\', '-- b'"), canonical("select E'a\\', '-- c'"));
  assert.notEqual(canonical('insert into t values ("a\\" -- b")'), canonical('insert into t values ("a\\" -- c")'));
  // MySQL runs `/*! … */`, and reads `n--1` as `n - -1`.
  assert.notEqual(canonical("create table t (a int) /*!50100 PARTITION BY HASH (a) */"), canonical("create table t (a int) /*!50100 PARTITION BY KEY (a) */"));
  assert.notEqual(canonical("select 1--1"), canonical("select 1--2"));
  assert.equal(canonical("select 1 --\tx\n, 2 --\n"), "select 1 , 2");
  // A comment Postgres nests and MySQL ends at its first `*/`.
  assert.notEqual(canonical("/* a /* b */ create table t (a int); /* */"), canonical("/* a /* b */ create table t (a bigint); /* */"));
  assert.equal(canonical("select /* a /* b */ c */  1"), "select /* a /* b */ c */  1");
  assert.equal(canonical("select /* a */  1"), "select 1");
  // MySQL's `#` comments, and a comment that never ends.
  assert.notEqual(canonical("# see reylo/*.sql\ncreate table t (a int);"), canonical("# see reylo/*.sql\ncreate table t (a bigint);"));
  assert.notEqual(canonical("select 1; /* create table t (a int);"), canonical("select 1; /* create table t (a bigint);"));
  assert.equal(canonical("select 1 /* a */"), "select 1");
  // A backslash both readings agree on changes nothing.
  assert.equal(canonical("select 'a\\n' -- x\n,  'b\\\\'"), "select 'a\\n' , 'b\\\\'");
});

test("line endings and trailing whitespace are no edit, and a statement's change is one", () => {
  assert.equal(normalized("a\r\nb\rc  \n\n"), "a\nb\rc");
  assert.ok(sameMigration("create table t (\r\n  a int -- the key\r\n);\r\n", "create table t (\n  a int\n);"));
  assert.ok(sameMigration("insert into t values ('a\r\nb');", "insert into t values ('a\nb');"));
  assert.ok(!sameMigration("create table t (a int);", "create table t (a bigint);"));
});

test("a space beyond ASCII is part of a name, as the databases read it", () => {
  assert.ok(!sameMigration("create view v as select 1 as x\u3000, 2 as y;", "create view v as select 1 as x , 2 as y;"));
  assert.ok(!sameMigration("create table t (a\u00a0int)", "create table t (a int)"));
  assert.ok(!sameMigration("select 1 as x\u3000", "select 1 as x"));
  assert.equal(canonical("select  'a\u3000b' -- x\n"), "select 'a\u3000b'");
  assert.ok(!sameMigration("select 1 --\u3000x", "select 1 --\u3000; drop table users;"));
  assert.ok(!sameMigration("select 1 --\u00a0x", "select 1 --\u00a0; drop table users;"));
  assert.ok(!sameMigration("select 1 --\u007fx", "select 1 --\u007f; drop table users;"));
});

test("a lone carriage return and a dollar quote's tag read as Postgres reads them", () => {
  assert.ok(!sameMigration("-- note\rdrop table users;", "-- note\ndrop table users;"));
  assert.ok(!sameMigration("insert into t values ('a\rb');", "insert into t values ('a\nb');"));
  assert.equal(canonical("select 1 -- a\r\n,\r2"), "select 1 , 2");
  assert.notEqual(canonical("select $\u3000$--\nx$\u3000$"), canonical("select $\u3000$ x$\u3000$"));
  assert.notEqual(canonical("select x$$a$, '$a$ -- b'"), canonical("select x$$a$, '$a$ -- c'"));
  const sqliteDefault = (word: string) => `create table t ([$\u3000$] text default 'a $\u3000$ -- ${word}');`;
  assert.ok(!sameMigration(sqliteDefault("one"), sqliteDefault("two")));
  assert.ok(!sameMigration("create table t ([a'b] text default 'x -- y')", "create table t ([a'b] text default 'x -- z')"));
  assert.equal(canonical("select data['key'],  arr[1] -- c\nfrom t"), "select data['key'], arr[1] from t");
});

// SQLite's statements that control the transaction, from the corpus `atomicity_problem` in
// engine/migrate/src/sql.rs is held to too, so that the two readers can't drift apart.
test("a SQLite migration's own begin, commit, end, rollback, savepoint or release is found as the command line finds it", () => {
  const corpus = JSON.parse(readFileSync(new URL("../engine/migrate/tests/atomicity.json", import.meta.url), "utf8")) as { cases: { sql: string; control: string | null }[] };
  for (const { sql, control } of corpus.cases) assert.equal(transactionControl(sql) ?? null, control, sql);
});

test("a migration that turns foreign keys on is found however SQLite reads the pragma", () => {
  for (const sql of ["pragma foreign_keys = on;", "PRAGMA \"foreign_keys\" = ON;", "pragma main.foreign_keys = 2;", "pragma foreign_keys = 01;", "pragma foreign_keys('yes');", "pragma foreign_keys = full;", "create table a (b); pragma [Foreign_Keys] = true;"]) assert.ok(turnsForeignKeysOn(sql), sql);
  for (const sql of ["pragma foreign_keys = off;", "pragma foreign_keys = 0;", "pragma foreign_keys;", "pragma foreign_key_check;", "-- pragma foreign_keys = on\ncreate table a (b);", "select 'pragma foreign_keys = on';"]) assert.ok(!turnsForeignKeysOn(sql), sql);
});
