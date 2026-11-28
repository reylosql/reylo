-- A column the catalog lets hold NULL may still refuse it: through a domain declared NOT NULL, a
-- domain that checks VALUE IS NOT NULL, or a CHECK that tests the column IS NOT NULL. Until
-- 2026-10-21 a bare parameter written whole to such a column was typed nullable, and the database
-- refused the NULL the caller was allowed to pass ("domain required_email does not allow null
-- values", a check violation). A bare parameter written whole, by VALUES, by the SELECT that feeds
-- an INSERT, by SET or by a row value, takes the column's nullability, read from its NOT NULL, its
-- domains and its CHECKs, so each one here is a value but `note`'s; a cast one is a value; one cast
-- to `nullable(…)` may be null. The oracle binds NULL to every parameter typed nullable and fails the
-- case where the database refuses it with a NOT NULL or a CHECK violation.
create domain required_email as text not null;
create domain present_text as text check (value is not null);
create table accounts (id int primary key, email required_email, nickname text check (nickname is not null), note text, tag present_text);
insert into accounts values (1, 'ada@example.com', 'ada', null, 'x');
-- A CHECK that is false wherever a column is NULL refuses NULL in that column, by whatever test:
-- `coalesce` with a default that fails it, IS NOT DISTINCT FROM, IS TRUE and NOT … IS NULL, as
-- much as IS NOT NULL. One that is true or NULL there lets NULL through, and so does one that
-- another column can make true. Each column's default passes its CHECK.
create table checked (id int primary key, a int default 1 check (coalesce(a, 0) > 0), b int default 1 check (b is not distinct from 1), c boolean default true check (c is true), d int default 1 check (not (d is null)), e numeric default 1 check (coalesce(e, 0) > 0), f int check (coalesce(f, 1) > 0), g boolean check (g is not false), h int check (h is null or h > 0), i int, j int default 1, k text check (k <> ''), check (i is not null or j is not null));
-- A generated column that refuses NULL, by its NOT NULL or a CHECK, refuses it in each column it
-- is NULL wherever that column is NULL: through CASE with no ELSE whose every test is NULL there,
-- or a strict operator. One that gives a value there lets NULL through, as CASE's ELSE and
-- `coalesce` do.
create table labels (id int primary key, p int, gp int generated always as (case when p is null then null else 1 end) stored not null, q int, gq text generated always as (case when q > 0 then 'p' when q <= 0 then 'n' end) stored not null, r int, gr int generated always as (r + 1) stored not null, s int, gs int generated always as (case when s > 0 then s else 0 end) stored not null, t int, gt int generated always as (t * 2) stored check (gt is not null), u int, gu int generated always as (coalesce(u, 0)) stored not null);
