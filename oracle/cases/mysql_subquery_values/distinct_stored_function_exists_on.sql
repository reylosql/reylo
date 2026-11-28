select distinct x.v from users u, (select exists(select 1 from singleton a join singleton b on b.n = a.n and pick() = p.id where a.n = 1 having 1 = 1) as v from posts p where p.id = 1) x
