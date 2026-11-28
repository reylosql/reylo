select distinct x.v from users u, (with c as (select pick() from posts) (select id, user_id as v from posts)) x where x.id = 1
