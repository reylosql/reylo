INSERT INTO users (id, name, role) SELECT number, 'filled', if({admin:Bool}, 'admin', 'member') FROM (SELECT number FROM numbers(3) WHERE number != 1 ORDER BY number WITH FILL)
