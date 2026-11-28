select u.id from users u, (select abs(random()) % 3 + 1 as id) x where u.id = x.id
