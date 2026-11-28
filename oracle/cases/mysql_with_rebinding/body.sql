-- z's t is the derived table's own, two rows, where alone it is one.
with t as (select 1 as x), z as (select x from t) select d.x from (with t as (select 2 as x union all select 3) select x from z) d
