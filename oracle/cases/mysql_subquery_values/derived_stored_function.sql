select distinct x.id from users u, (select pick() as id from posts where posts.id = 1) x
