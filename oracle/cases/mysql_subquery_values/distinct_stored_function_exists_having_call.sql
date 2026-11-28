select distinct x.v from users u, (select exists(select 1 from singleton where n = 1 having pick() = p.id) as v from posts p where p.id = 1) x
