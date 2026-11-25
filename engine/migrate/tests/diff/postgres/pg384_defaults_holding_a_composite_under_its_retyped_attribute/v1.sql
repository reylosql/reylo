create type p as (a integer, b text);
create table t (id integer primary key, c p default row(0, 'zero')::p);
create table u (id integer primary key, n integer default (row(7, 'seven')::p).a, shown text default (row(1, 'one')::p)::text);
