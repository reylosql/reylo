select id from users where id in (select user_id from public.recent_posts where id = :id::int)
