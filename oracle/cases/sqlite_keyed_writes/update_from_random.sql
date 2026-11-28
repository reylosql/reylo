update users set name = 'x' from (select (random() & 1) + 1 as id from accounts where accounts.id = 1) x where users.id = x.id returning users.id
