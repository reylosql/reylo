create type "Mood" as enum ('a', 'b|c', 'q"r', 'd.e');
create table "t0,x" (
  id integer not null,
  "c2,x" numeric default 7.5 not null,
  constraint "t0,x_pkey" primary key (id)
);
create table h0 (
  id integer,
  "e0,x" "Mood"[],
  "e1.x" text[]
) inherits ("t0,x");
