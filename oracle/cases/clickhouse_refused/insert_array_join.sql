INSERT INTO users (id, age) SELECT 1, wide FROM users ARRAY JOIN [{x:UInt64}] AS wide
