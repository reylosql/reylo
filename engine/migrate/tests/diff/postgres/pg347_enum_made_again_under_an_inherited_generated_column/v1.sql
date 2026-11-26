create type "mood,x" as enum ('a', 'b|c', 'q"r', 'd.e');
create function f1(a0 integer, a1 smallint) returns "mood,x" language sql immutable return ('a')::"mood,x";
create table "H0" (
  id integer not null,
  g0 "mood,x" generated always as (f1((id)::integer, (7)::smallint)) stored,
  constraint "H0_pkey" primary key (id)
);
create table h1 (
  id integer,
  constraint h1_pkey primary key (id)
) inherits ("H0");
