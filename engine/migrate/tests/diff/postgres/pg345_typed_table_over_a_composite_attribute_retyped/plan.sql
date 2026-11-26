alter type mood add value 'x0' after 'q"r';
create type ty0 as (
  id integer,
  c0 integer,
  "c1.x" mood,
  "n0.x" p0
);
alter table t0 alter column "n0.x" type text using "n0.x"::text;
alter type p0 alter attribute "a2""x" type bigint;
alter table t0 alter column "n0.x" type p0 using "n0.x"::text::p0;
alter table t0 of ty0;
