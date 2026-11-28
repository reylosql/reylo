select id from users where email = (select title from posts where id = :id)
