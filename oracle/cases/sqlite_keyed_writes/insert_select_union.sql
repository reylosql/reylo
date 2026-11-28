insert into accounts (id, name) select :a, 'x' union all select :b, 'y' returning id
