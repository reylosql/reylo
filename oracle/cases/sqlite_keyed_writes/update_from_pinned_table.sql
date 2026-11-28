update users set name = 'x' from notes n where n.id = :id and users.id = n.user_id returning users.id
