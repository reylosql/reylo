INSERT INTO users (id, role) SELECT 1, if({admin:Bool}, 'admin', 'member') AS chosen FROM users GROUP BY chosen WITH TOTALS
