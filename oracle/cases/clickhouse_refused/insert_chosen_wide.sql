INSERT INTO users (id, name, age) SELECT {id:UInt64}, 'x', if({take:Bool}, id, 0) FROM users
