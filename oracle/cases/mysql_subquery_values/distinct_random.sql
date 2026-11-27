select distinct x.id from users u, (select cast(floor(rand() * 3) + 1 as signed) as id from posts where posts.id = 1) x
