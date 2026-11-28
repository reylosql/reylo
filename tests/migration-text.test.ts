import { test } from "node:test";
import assert from "node:assert/strict";
import { canonical, normalized, sameMigration, transactionControl } from "../src/migration-text.ts";

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

// SQLite's statements that control the transaction, as `atomicity_problem` in engine/migrate/src/sql.rs finds them.
test("a SQLite migration's own begin, commit, end, rollback, savepoint or release is found, outside quotes, comments and a trigger's body", () => {
  assert.equal(transactionControl("update a set b = 1; commit; insert into c values (1);"), "commit");
  assert.equal(transactionControl("BEGIN; create table x (a);"), "begin");
  assert.equal(transactionControl("create table \"end\" (a); savepoint s;"), "savepoint");
  assert.equal(transactionControl("create trigger t after insert on x begin select 1; end; end;"), "end");
  for (const sql of [
    "create trigger t after insert on x begin update y set a = case when 1 then 2 end; end; create table z (a);",
    "create temp trigger t after insert on x begin select raise(rollback, 'no'); end;",
    "create table x (a); -- commit\nselect 1;",
    "/* rollback */ create table x (a);",
    "insert into t values ('; commit;'), ('it''s; end');",
    "select [begin; end] from t;",
    "create table users (id integer primary key);\n--Rollback: drop table users;\n",
    "create table a (b int); -- don't\ncreate table c (d int);",
    "create trigger t after update on s begin update s set total = total + new.end - new.start; end; create table x (a);",
  ]) assert.equal(transactionControl(sql), undefined, sql);
  // An apostrophe in a comment opens no string that hides the commit after it.
  assert.equal(transactionControl("create table a (b int); -- don't\ncommit;"), "commit");
  assert.equal(transactionControl("create trigger t after update on s when new.begin > 0 begin select 1; end; commit;"), "commit");
});
