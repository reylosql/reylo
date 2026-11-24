DELETE FROM orders IN PARTITION tuple() WHERE id = {id:UInt64}
