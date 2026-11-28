insert into accounts (id, name) select a.id + 100, 'x' from accounts a cross join (select (random() & 1) + 1 as id from accounts where accounts.id = 1) x where a.id = x.id returning id
