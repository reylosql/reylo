INSERT INTO orders SELECT * EXCEPT (placed), {placed:Date} FROM orders
