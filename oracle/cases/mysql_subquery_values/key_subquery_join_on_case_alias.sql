select id from users u, singleton U where n = 1 and id = (select p.user_id from posts u, posts p join users s on s.id = u.id and s.id = p.user_id limit 1)
