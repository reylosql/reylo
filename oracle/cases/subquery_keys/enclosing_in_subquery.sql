with a as (select id, title from posts) select * from (select id, title from a where id = :id) s
