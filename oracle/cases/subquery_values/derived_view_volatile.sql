select u.id from users u join (select id from numbered_posts where post_id = 1 limit 1) x on x.id = u.id
