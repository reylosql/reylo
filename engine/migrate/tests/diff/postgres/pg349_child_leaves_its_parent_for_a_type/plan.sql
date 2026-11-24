-- loses data: "t0,x"
create type "ty0""x" as (
  id integer,
  "e0,x" "Mood"[],
  "e1.x" text[]
);
alter table h0 alter column id drop not null;
alter table h0 no inherit "t0,x";
alter table h0 alter column id drop not null;
alter table h0 of "ty0""x";
drop table "t0,x";
