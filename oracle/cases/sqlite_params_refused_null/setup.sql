-- A column that may hold NULL by its declaration may still refuse it through a CHECK. Until
-- 2026-10-21 a bare parameter written whole to such a column was typed nullable, and SQLite refused
-- the NULL ("CHECK constraint failed"). A bare parameter written whole takes the column's
-- nullability, read from its NOT NULL and its CHECKs, so each one here is a value but `note`'s; one
-- cast to `nullable(…)` may be null. The oracle fails a case whose NULL the database refuses.
create table accounts (id integer primary key, email text check (email is not null), nickname text check (nickname is not null), note text);
insert into accounts values (1, 'ada@example.com', 'ada', null);
-- A CHECK that is false wherever a column is NULL refuses NULL in that column, by whatever test:
-- `coalesce` or `ifnull` with a default that fails it, IS, IS TRUE and NOT … IS NULL, as much as
-- IS NOT NULL. One that is true or NULL there lets NULL through, and so does one that another
-- column can make true. `coalesce` gives an integer as it is, and 10 is not below 9, so `k`
-- refuses NULL. Each column's default passes its CHECK.
create table checked (id integer primary key, a int default 1 check (coalesce(a, 0) > 0), b int default 1 check (b is 1), c int default 1 check (c is true), d int default 1 check (not (d is null)), e int default 1 check (ifnull(e, 0) > 0), f int check (coalesce(f, 1) > 0), g int check (g is not false), h int check (h is null or h > 0), i int, j int default 1, k text default 'x' check (coalesce(k, 10) < coalesce(k, 9) or k is not null), check (i is not null or j is not null));
-- A generated column that refuses NULL, by its NOT NULL or a CHECK, refuses it in each column it
-- is NULL wherever that column is NULL, through `iif`, a CASE with no ELSE whose every test is
-- NULL there, a strict operator, and the generated columns it reads in turn. One that gives a
-- value there lets NULL through, as `iif`'s other branch and `coalesce` do.
create table labels (id integer primary key, p int, gp int generated always as (iif(p is null, null, 1)) not null, q int, gq text generated always as (case when q > 0 then 'p' when q <= 0 then 'n' end) not null, r int, gr int generated always as (r + 1), gr2 int generated always as (gr * 2) stored not null, s int, gs int generated always as (iif(s > 0, s, 0)) not null, t int, gt int generated always as (t + 1) check (gt is not null), u int, gu int generated always as (coalesce(u, 0)) not null);
