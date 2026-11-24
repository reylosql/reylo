INSERT INTO orders (id, user_id, total, placed) SETTINGS async_insert = 1, wait_for_async_insert = 1 VALUES ({id:UInt64}, {user:UInt64} + 0, 1.5, {placed:Date})
