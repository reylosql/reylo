insert into accounts (id, name) select id + 100, name from accounts where id = :id returning id
