set check_function_bodies = off;
drop function blank();
drop function blank_id();
alter table t alter column n drop default;
alter table t alter column n type bigint;
alter table t alter column n set default zero();
create function blank() returns t language sql stable return row(0, 0)::t;
create function blank_id() returns integer language sql stable return (row(0, 0)::t).id;
reset check_function_bodies;
