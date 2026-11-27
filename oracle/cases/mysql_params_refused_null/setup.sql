-- A column that may hold NULL by its declaration may still refuse it through a CHECK (MySQL
-- 8.0.16 and later). Until 2026-10-21 a bare parameter written whole to such a column was typed
-- nullable, and MySQL refused the NULL (error 3819). A bare parameter written whole takes the
-- column's nullability, read from its NOT NULL and its enforced CHECKs, so each one here is a value
-- but `note`'s; one cast to `nullable(…)` may be null. The oracle fails a case whose NULL the database refuses
-- with a NOT NULL (1048) or a CHECK (3819) error.
create table accounts (id int primary key, email varchar(40) check (email is not null), nickname varchar(20) check (nickname is not null), note varchar(40));
insert into accounts values (1, 'ada@example.com', 'ada', null);
-- A NOT NULL generated column refuses NULL in what it computes, so a column its expression is NULL
-- for wherever that column is NULL refuses it too (error 1048, naming the generated column), and a
-- column it reads through `coalesce` does not.
create table measures (id int primary key, nn int not null, nu int, g int generated always as (nu + 1) stored not null, nv int, h int generated always as (coalesce(nv, 0)) virtual not null, ns varchar(10), j varchar(12) generated always as (concat('x', ns)) virtual not null);
-- Each generated column of `computed` reads a column of its own. The expression is NULL wherever
-- that column is NULL through a function or operator measured strict in it (a JSON path, a string,
-- date or hash function, a comparison, TRIM and SUBSTRING), so the column refuses NULL; through
-- `concat_ws` after its separator, `ifnull`, or `json_set`'s value it may be NULL.
create table computed (id int primary key, j json, a text generated always as (json_unquote(json_extract(j, '$'))) virtual not null, k json, b text generated always as (k->>'$') virtual not null, e varchar(40), c varchar(40) generated always as (substring_index(e, '@', -1)) virtual not null, t varchar(20), d varchar(20) generated always as (lower(replace(t, ' ', '-'))) virtual not null, born date, f int generated always as (to_days(born)) virtual not null, pw varchar(20), h char(32) generated always as (md5(pw)) virtual not null, n int, positive boolean generated always as (n > 0) virtual not null, raw varchar(20), trimmed varchar(20) generated always as (trim(raw)) virtual not null, code varchar(20), prefix varchar(3) generated always as (substring(code, 1, 3)) virtual not null, due date, dom int generated always as (dayofmonth(due)) virtual not null, tag varchar(10), tags varchar(30) generated always as (concat_ws(',', 'x', tag)) virtual not null, hint varchar(10), shown varchar(10) generated always as (ifnull(hint, '')) virtual not null, m int, jm json generated always as (json_set('{}', '$.a', m)) virtual not null);
-- A CHECK that is false wherever a column is NULL refuses NULL in that column (error 3819), by
-- whatever test: `coalesce` or `ifnull` with a default that fails it, `<=>`, IS TRUE and `isnull`,
-- as much as IS NOT NULL. One that is true or NULL there lets NULL through, and so does one that
-- another column can make true, and `k`'s: `coalesce` over a VARCHAR gives a string, and MySQL
-- orders two strings as text, '10' before '9'. Each column's default passes its CHECK.
create table checked (id int primary key, a int default 1 check (coalesce(a, 0) > 0), b int default 1 check (b <=> 1), c int default 1 check (c is true), d int default 1 check (isnull(d) = 0), e int default 1 check (ifnull(e, 0) > 0), f int check (coalesce(f, 1) > 0), g int check (g is not false), h int check (h is null or h > 0), i int, j int default 1, check (i is not null or j is not null), k varchar(10) check (coalesce(k, 10) < coalesce(k, 9) or k is not null));
-- A NOT NULL generated column is NULL wherever a column it reads is NULL through IF, a CASE with no
-- ELSE whose every test is NULL there, or NULLIF over a test for NULL, so that column refuses NULL
-- too (error 1048). One that gives a value there lets NULL through, as IF's other branch and CASE's
-- ELSE do where a test is NULL, and so does one that takes its value there from another column.
create table labels (id int primary key, p int, gp int generated always as (if(p is null, null, 1)) stored not null, q int, gq varchar(1) generated always as (case when q > 0 then 'p' when q <= 0 then 'n' end) virtual not null, r int, gr int generated always as (nullif(1, r is null)) stored not null, s int, gs int generated always as (if(s > 0, s, 0)) virtual not null, t int, gt varchar(1) generated always as (case when t > 0 then 'p' else 'n' end) virtual not null, u int, gu int generated always as (nullif(2, u is null)) virtual not null, v int, gv int generated always as (v <=> 1) virtual not null, w int, gw int generated always as (w is null) virtual not null, y int, z int default 1, gz int generated always as (if(y is null, z, 1)) virtual not null);
