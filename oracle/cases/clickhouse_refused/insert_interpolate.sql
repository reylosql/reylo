INSERT INTO users (id, age) SELECT id, age FROM users WHERE id = 1 ORDER BY id WITH FILL FROM 1 TO 3 INTERPOLATE (age AS {x:UInt64})
