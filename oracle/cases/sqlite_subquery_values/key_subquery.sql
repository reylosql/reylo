select id from users where id = (select user_id from posts where id = :id)
