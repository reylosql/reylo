INSERT INTO users (id, role) SELECT number, if({admin:Bool}, 'admin', 'member') FROM numbers(3) WHERE number != 1 ORDER BY number WITH FILL
