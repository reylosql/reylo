INSERT INTO orders SELECT * REPLACE ({total:Decimal(18, 4)} AS total) FROM orders
