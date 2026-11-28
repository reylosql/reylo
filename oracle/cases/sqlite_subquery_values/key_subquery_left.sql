select id from users where (select max(user_id) from posts) = id
