select distinct x.v from users u, ((select id, pick() as v from posts)) x where x.id = 1
