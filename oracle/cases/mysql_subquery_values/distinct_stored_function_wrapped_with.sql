select distinct x.v from users u, (with c as (select id, pick() as v from posts) (select id, v from c)) x where x.id = 1
