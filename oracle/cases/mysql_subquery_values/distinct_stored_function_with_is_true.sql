select distinct x.v from users u, (with c as (select id, pick() as r from posts) select (exists(select 1 from c where c.id = p.id and c.r = 1) is true) as v from posts p where p.id = 1) x
