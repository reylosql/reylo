set check_function_bodies = off;
alter table "H0" drop column g0;
drop function f1(integer,smallint);
alter type "mood,x" rename to "mood,x__old";
create type "mood,x" as enum ('a', 'b|c', 'q"r');
drop type "mood,x__old";
create function f1(a0 integer, a1 smallint) returns "mood,x" language sql immutable return ('a')::"mood,x";
alter table "H0" add column g0 "mood,x" generated always as (f1(id::int, 7::smallint)) stored;
reset check_function_bodies;
