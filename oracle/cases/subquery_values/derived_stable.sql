select u.id from users u, (select user_id as id from posts where posts.id = 1) x where u.id = x.id
