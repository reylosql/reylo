insert into accounts (id, name) select max(id) + 1, 'x' from accounts returning id
