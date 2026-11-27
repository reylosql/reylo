-- A JSON shape reads z's columns as the subquery's own WITH makes them.
with z as (select p.x from t3 left join t4 p on p.k = t3.k where p.k is not null) select (with t4 as (select k, cast(null as signed) as x from t3) select json_arrayagg(z.x) from z) as j
