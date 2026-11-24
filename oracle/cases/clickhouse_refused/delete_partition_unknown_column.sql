DELETE FROM orders IN PARTITION tuple() WHERE nope = {id:UInt64}
