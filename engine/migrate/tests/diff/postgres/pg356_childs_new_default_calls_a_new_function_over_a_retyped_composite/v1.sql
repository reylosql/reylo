create type p as (a int);
create table t (id int, c p);
create table h (c p default row(1)::p) inherits (t);
