INSERT INTO users (id, name, role) SELECT {id:UInt64}, 'y', multiIf({level:UInt8} > 8, 'admin', 'member')
