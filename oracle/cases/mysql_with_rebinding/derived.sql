with z as (select p.x from t3 left join t4 p on p.k = t3.k where p.k is not null) select d.x from (with t4 as (select k, cast(null as signed) as x from t3) select z.x from z) d
