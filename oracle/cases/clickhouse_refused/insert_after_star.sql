INSERT INTO orders SELECT * EXCEPT (placed), {placed:DateTime} FROM orders
