select distinct x.v from users u, (select floor(pick()) as v from posts where posts.id = 1) x
