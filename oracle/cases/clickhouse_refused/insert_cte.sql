INSERT INTO users (id, age) WITH c AS (SELECT {x:UInt64} AS v) SELECT 1, v FROM c
