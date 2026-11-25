alter table u alter column x drop default;
alter table t alter column n type bigint;
-- u.x is renamed y, keeping its rows. If they are two columns, write instead:
--   alter table u drop column x;
--   alter table u add column y text default row(0, 0)::t::text;
alter table u rename column x to y;
alter table u alter column y set default row(0, 0)::t::text;
