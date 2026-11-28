update users set name = 'x' from notes n where users.id = :id and n.user_id = users.id returning users.id
