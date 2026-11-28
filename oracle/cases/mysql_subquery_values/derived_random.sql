select u.id from users u, (select floor(rand() * 2) + 1 as id from posts where posts.id = 1) x where u.id = x.id
