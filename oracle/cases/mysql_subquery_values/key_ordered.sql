select id from users where id = (select user_id from posts where user_id > :n order by id limit 1)
