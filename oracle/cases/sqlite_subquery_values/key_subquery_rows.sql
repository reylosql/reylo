select id from users where id = (select user_id from posts order by id)
