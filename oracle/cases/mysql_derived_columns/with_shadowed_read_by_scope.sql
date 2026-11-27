-- The outer WITH query z reads the outer posts too, which the derived table's own posts hides.
with posts as (select email as title from users), z as (select title from posts) select d.t from (with posts as (select 'x' as title) select z.title as t from z) d
