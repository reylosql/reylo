insert into accounts (id, name) select :id, 'x' where :ok returning id
