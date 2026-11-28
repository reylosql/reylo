delete from notes where id in (select id from notes where user_id = :u) returning id
