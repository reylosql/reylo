SELECT id FROM users ORDER BY id LIMIT (SELECT count() FROM orders WHERE id = 10)
