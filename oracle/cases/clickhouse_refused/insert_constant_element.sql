INSERT INTO users (id, age) SELECT 3, tupleElement(tuple(1, {x:UInt64}), {i:UInt8})
