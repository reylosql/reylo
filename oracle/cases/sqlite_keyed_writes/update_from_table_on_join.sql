update users set name = 'x' from notes n where users.id = n.user_id returning users.id
