select x.id, count(*) as n from users u, (select pick() as id from posts where posts.id = 1) x group by x.id
