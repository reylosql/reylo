select distinct x.v from users u, (select pick() in (select 1) as v from posts where id = 1) x
