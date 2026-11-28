select distinct x.v from users u, (select exists(select pick() as r from singleton where n = 1 having r = p.id) as v from posts p where p.id = 1) x
