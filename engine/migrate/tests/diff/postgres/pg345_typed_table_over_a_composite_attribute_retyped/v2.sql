create type mood as enum ('a', 'b|c', 'q"r', 'x0', 'd.e');
create type p0 as (a0 text, a1 bigint, "a2""x" bigint);
create type ty0 as (id integer, c0 integer, "c1.x" mood, "n0.x" p0);
create table t0 of ty0 (
  id with options not null,
  c0 with options default 7,
  "c1.x" with options default 'a',
  constraint t0_pkey primary key (id)
);
