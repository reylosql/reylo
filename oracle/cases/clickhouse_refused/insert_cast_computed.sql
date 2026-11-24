INSERT INTO users (id, age) SELECT 60, CAST({x:UInt64}, if({i:UInt8} = 12, 'UInt64', 'UInt8'))
