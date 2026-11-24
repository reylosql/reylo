ALTER TABLE users UPDATE role = if({admin:Bool}, 'admin', role) WHERE id = {id:UInt64}
