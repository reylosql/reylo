select id from users where id in (select user_id from posts order by id limit 1)
