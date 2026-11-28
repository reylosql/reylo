select id from users where id in (select user_id from posts where id = :id)
