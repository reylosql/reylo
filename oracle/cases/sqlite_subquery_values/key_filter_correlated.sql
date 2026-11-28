select id from users u where id = (select max(p.user_id) filter (where p.title = u.name) from posts p)
