-- In a RECURSIVE WITH, a WITH query sees no name declared after it: a's t4 is the one around.
with recursive z as (with recursive a as (select p.x from t3 left join t4 p on p.k = t3.k where p.k is not null), t4 as (select 1 as k, 1 as x) select x from a) select d.x from (with t4 as (select k, cast(null as signed) as x from t3) select z.x from z) d
