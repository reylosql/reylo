-- MySQL compares table names and aliases by case where lower_case_table_names is 0, as on Linux:
-- `t A` and `t a` are two FROM items, `E` and `e` two tables, and so are two derived tables or
-- WITH queries named `D` and `d`, and `with c as (…)` leaves a table `C` to be read. A server that
-- folds names (1 on Windows, 2 on macOS) refuses the second table of each pair, so this case builds
-- only on one that keeps case.
create table t (id int primary key, c int not null);
create table E (id int primary key, x enum('big1', 'big2') not null, n int, b bigint);
create table e (id int primary key, x enum('small1', 'small2') not null, n int not null, b int);
create table C (id int primary key, x enum('table1', 'table2') not null);
create table J (id int primary key, doc json);
create table j (id int primary key, doc json check (json_schema_valid('{"type": "object", "required": ["a"], "properties": {"a": {"type": "integer"}}}', doc)));
-- A table's unique keys, a view's statement and a WITH query are each read under the name the
-- server reads, never under one that differs from it in case alone: `kn` has no key beside `KN`,
-- `vw` is a table beside the view `VW`, `WS` reads `KS`, which has no key, beside `ks`, and
-- `with CW as (…)` leaves the table `cw` to be read. A qualifier reaches the one item spelled
-- the same: `x.*` beside `X` is `kx`'s and `e`'s, and `A.id` inside a LATERAL derived table
-- beside `a` is the enclosing query's `A`. A schema that differs from this one in case alone is
-- another: its `st` has no key and holds the same `id` twice.
create table KN (id int primary key, v int);
create table kn (id int, v int);
create table PN (id int, v int);
create table pn (id int primary key, v int);
create table vt (id int primary key, v int);
create table vw (id int, v int);
create view VW as select id, v from vt where id = 1;
create table ks (id int primary key, v int);
create table KS (id int, v int);
create view WS as select id, v from KS;
create table KW (id int primary key, v int);
create table cw (id int, v int);
create table DW (id int, v int);
create table KX (id int primary key);
create table kx (kid int, u int unique);
-- A view of `KN` gives no key to `kn` named with this schema, which the view's reading leaves unfetched.
create view knv as select id, v from KN where id = 1;
create table lo (id int primary key, v int);
create table LI (id int primary key, v int);
drop database if exists REYLO_ORACLE_MYSQL_NAME_CASE;
create database REYLO_ORACLE_MYSQL_NAME_CASE;
create table REYLO_ORACLE_MYSQL_NAME_CASE.st (id int, v int);
insert into REYLO_ORACLE_MYSQL_NAME_CASE.st values (1, 1), (1, 2);
create table st (id int primary key, v int);
create view sone as select id, v from st where id = 1;
create view stwin as select id, v from REYLO_ORACLE_MYSQL_NAME_CASE.st;
