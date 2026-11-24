SELECT count() AS n FROM users LIMIT (SELECT count() FROM orders WHERE id = 10)
