-- Where names compare as written, z's TU leaves its tu free, which the derived table's own then is.
with z as (with TU as (select 1 as id, 99 as x) select x from tu where id = 1) select d.x from (with tu as (select 1 as id, 10 as x union all select 1, 20) select x from z) d
