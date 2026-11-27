select id from users u where id in (select p.user_id from posts p where p.title = u.name limit 1)
