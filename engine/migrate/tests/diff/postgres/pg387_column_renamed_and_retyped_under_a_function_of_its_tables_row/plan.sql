set check_function_bodies = off;
drop function label(t);
-- t.n is renamed m, keeping its rows. If they are two columns, write instead:
--   alter table t drop column n;
--   alter table t add column m bigint;
alter table t rename column n to m;
alter table t alter column m type bigint;
create function label(x t) returns text language sql stable return x.id::text;
reset check_function_bodies;
