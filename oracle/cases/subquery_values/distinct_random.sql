select distinct x.id from users u, (select (random() * 2)::int + 1 as id from posts where posts.id = 1) x
