-- Read inside the derived table, z's tu is the derived table's own, two rows of id 1: tu's key bounds nothing.
with z as (select x from tu where id = 1) select d.x from (with tu as (select 1 as id, 1 as x union all select 1, 2) select z.x from z) d
