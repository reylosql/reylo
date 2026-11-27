-- The inner WITH query y reads the outer posts, which the inner one of the name hides only after
-- it: read alone without the outer one, the name would be the table posts, whose title is never
-- NULL.
with posts as (select email as title from users) select d.t from (with y as (select title as t from posts), posts as (select 1 as title) select t from y) d
