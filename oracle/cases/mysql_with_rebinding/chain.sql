-- e reads y, y reads z, and z's tu is e's own: across the WITH between them.
with z as (select x from tu where id = 1) select d.x from (with y as (select x from z) select e.x from (with tu as (select 1 as id, 1 as x union all select 1, 2) select x from y) e) d
