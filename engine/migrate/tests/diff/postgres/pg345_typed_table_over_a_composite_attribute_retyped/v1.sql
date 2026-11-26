create type mood as enum ('a', 'b|c', 'q"r', 'd.e');
create type p0 as (a0 text, a1 bigint, "a2""x" integer);
create table t0 (
  id integer not null,
  c0 integer default 7,
  "c1.x" mood default 'a',
  "n0.x" p0,
  constraint t0_pkey primary key (id)
);
