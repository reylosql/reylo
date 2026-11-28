select u.id from users u where u.id = (select p.user_id from posts p join singleton s on s.n = p.id where p.id = :id)
