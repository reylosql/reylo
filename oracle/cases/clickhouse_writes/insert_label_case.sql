INSERT INTO users (id, name, role) VALUES ({id:UInt64}, 'z', CASE {level:UInt8} WHEN 9 THEN 'admin' ELSE 'member' END)
