insert or ignore into accounts (id, name) select :id, 'x' returning id
