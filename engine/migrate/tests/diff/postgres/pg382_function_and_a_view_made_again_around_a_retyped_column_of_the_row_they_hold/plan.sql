set check_function_bodies = off;
drop function blank();
drop view blanks;
alter table t alter column n type bigint;
create view blanks as select row(0, 0)::t as r;
create function blank() returns t language sql stable return row(0, 0)::t;
reset check_function_bodies;
