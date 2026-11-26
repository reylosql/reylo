create type p as (a bigint);
create function new_default() returns p language sql immutable return row(2)::p;
create table t (id integer, c p);
create table h (c p default new_default()) inherits (t);
