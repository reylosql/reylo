select distinct x.id from users u, (select abs(random()) % 3 + 1 as id) x where x.id = 1
