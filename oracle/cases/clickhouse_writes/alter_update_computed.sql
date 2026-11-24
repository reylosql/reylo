ALTER TABLE orders UPDATE total = total + {more:Decimal(18, 2)} WHERE id = {id:UInt64}
