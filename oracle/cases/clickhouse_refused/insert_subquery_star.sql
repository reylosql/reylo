INSERT INTO users (id, age) SELECT * FROM (SELECT 11 AS id, {x:UInt64} AS a)
