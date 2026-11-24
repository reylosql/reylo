INSERT INTO users (id, name, role) SELECT {id:UInt64}, 'x', if({admin:Bool}, 'admin', lower({role:String}))
