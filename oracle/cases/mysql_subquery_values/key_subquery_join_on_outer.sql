select u.id from users u where u.id = (select p.user_id from input, posts p join singleton s on p.title = name where input.name = 'a' and s.n = 1 limit 1)
