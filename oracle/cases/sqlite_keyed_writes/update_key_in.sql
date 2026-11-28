update users set name = 'x' where id in (select user_id from notes where id = :id) returning id
