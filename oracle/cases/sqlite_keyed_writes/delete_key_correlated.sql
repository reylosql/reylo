delete from notes where id = (select max(n.id) from notes n where n.user_id = notes.user_id) returning id
