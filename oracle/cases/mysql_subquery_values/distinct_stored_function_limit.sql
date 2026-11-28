select distinct x.v from users u, (select pick() as v from posts limit 1) x
