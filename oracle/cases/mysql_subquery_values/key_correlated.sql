select id from users u where id = (select p.user_id from posts p where p.title = u.name)
