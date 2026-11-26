alter table h alter column c drop default;
drop function new_default();
alter table only h alter column c drop default;
alter table t alter column c type text using c::text;
alter type p alter attribute a type integer;
alter table t alter column c type p using c::text::p;
alter table h alter column c set default row(1)::p;
