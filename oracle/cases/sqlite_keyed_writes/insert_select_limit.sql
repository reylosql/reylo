insert into accounts (id, name) select id + 100, name from accounts order by id limit 1 returning id
