-- The derived table's own t4 hides the outer one, which z, written before it, reads as the table.
with z as (select t4.k, t4.x from t4), t4 as (select 1 as k, 1 as x) select d.x from (with t4 as (select k, cast(null as signed) as x from t3) select z.x from t3 left join z on z.k = t3.k where z.k is not null) d
