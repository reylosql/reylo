select id from users u where id = (select p.user_id from posts p order by abs(p.id - u.id) limit 1)
