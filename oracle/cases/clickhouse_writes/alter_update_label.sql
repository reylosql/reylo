ALTER TABLE users UPDATE role = if({admin:Bool}, 'admin', 'member') WHERE id = {id:UInt64}
