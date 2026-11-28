select id from users u where id = (select user_id from posts p where p.title = u.name)
