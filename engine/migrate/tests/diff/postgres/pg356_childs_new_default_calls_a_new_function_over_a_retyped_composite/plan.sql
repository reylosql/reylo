set check_function_bodies = off;
alter table h alter column c drop default;
alter table only h alter column c drop default;
alter table t alter column c type text using c::text;
alter type p alter attribute a type bigint;
alter table t alter column c type p using c::text::p;
create function new_default() returns p language sql immutable as 'select row(2)::p';
alter table h alter column c set default new_default();
reset check_function_bodies;
