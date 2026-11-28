update users set name = 'x' where id = (select max(n.user_id) filter (where n.body = users.name) from notes n) returning id
