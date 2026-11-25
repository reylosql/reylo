create type p0 as (a0 text, "A1" numeric, "a2.x" bigint);
create table t0 (
  id integer not null,
  "N0" p0,
  constraint t0_pkey primary key (id)
);
create table h0 (
  id integer,
  constraint h0_pkey primary key (id)
) inherits (t0);
