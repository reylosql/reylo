select distinct x.v from users u, (select (select pick()) as v from posts where id = 1) x
