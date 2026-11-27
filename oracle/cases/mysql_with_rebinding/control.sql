-- With no WITH of the name inside, the outer WITH query's test still proves its column.
with z as (select p.x from t3 left join t4 p on p.k = t3.k where p.k is not null) select d.x from (with u as (select 1 as k) select z.x from z, u) d
