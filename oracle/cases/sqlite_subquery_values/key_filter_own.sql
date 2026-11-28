select id from users where id = (select max(p.user_id) filter (where p.title = 'z') from posts p)
