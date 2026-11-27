-- A WITH of the sibling y, which reads no outer WITH query, leaves z as it is.
with z as (select p.x from t3 left join t4 p on p.k = t3.k where p.k is not null), y as (with p as (select 1 as k) select k from p) select z.x from z, y
