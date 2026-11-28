delete from notes where id = (select max(id) from notes) returning id
