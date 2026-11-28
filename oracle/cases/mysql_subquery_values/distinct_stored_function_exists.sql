select distinct x.v from users u, (select exists(select pick() from posts) as v from posts where id = 1) x
