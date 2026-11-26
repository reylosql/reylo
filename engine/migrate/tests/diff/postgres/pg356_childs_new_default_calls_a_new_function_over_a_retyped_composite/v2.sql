create type p as (a bigint);
create function new_default() returns p language sql immutable as 'select row(2)::p';
create table t (id int, c p);
create table h (c p default new_default()) inherits (t);
