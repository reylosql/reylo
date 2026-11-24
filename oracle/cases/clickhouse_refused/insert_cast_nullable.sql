INSERT INTO users (id, age) SELECT 92, CAST(tupleElement(tuple(1, {x:Nullable(UInt8)}), {i:UInt8}) AS UInt8) SETTINGS cast_keep_nullable = 1
