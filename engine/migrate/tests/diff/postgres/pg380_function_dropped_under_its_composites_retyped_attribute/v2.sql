create type p as (a integer);
create table t (id integer, c p);
create table h (c p default row(1)::p) inherits (t);
