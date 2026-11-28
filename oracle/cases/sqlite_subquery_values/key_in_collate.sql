select id from users where email in (select title collate nocase from posts where id = :id)
