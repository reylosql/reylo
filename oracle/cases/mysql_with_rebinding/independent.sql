-- Only z is rebound inside the EXISTS: only_one still returns one row.
with only_one as (select 1 as x), z as (select x from tu) select o.x from only_one o where exists (with tu as (select 20 as x union all select 30) select 1 from z where x = 20)
