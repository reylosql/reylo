select x.id, count(*) as n from users u, (select cast(floor(rand() * 3) + 1 as signed) as id from posts where posts.id = 1) x group by x.id
