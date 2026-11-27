select id from users where id = (select user_id from recent_posts where id = :id::int)
