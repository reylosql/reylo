set check_function_bodies = off;
alter table h alter column n drop default;
alter table h alter column n drop default;
alter table h alter column n type bigint;
alter table t alter column n type bigint;
create function g(r t) returns bigint language sql stable return (select coalesce(max(n), 0) from h) + r.id;
alter table h alter column n set default g(row(0, 0)::t);
reset check_function_bodies;
