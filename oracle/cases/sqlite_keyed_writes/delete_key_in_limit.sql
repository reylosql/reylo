delete from notes where id in (select id from notes where user_id = :u order by id limit 1) returning id
