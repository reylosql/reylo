INSERT INTO orders (id, user_id) SELECT {id:UInt64}, 1 UNION ALL (SELECT 2, {user:UInt64})
