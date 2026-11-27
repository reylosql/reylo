select id from users where id = any (select user_id from posts where id = :id::int)
