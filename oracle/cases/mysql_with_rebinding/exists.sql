-- A WITH in a subquery that reads no outer WITH query leaves tu's key bounding z.
with z as (select x from tu where id = 1) select z.x from z where exists (with q as (select 1) select 1 from q)
