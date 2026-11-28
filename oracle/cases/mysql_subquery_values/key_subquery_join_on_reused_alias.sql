select u.id from users u where u.id = (select p.user_id from posts u, posts p join singleton s on s.n = u.id limit 1)
