INSERT INTO orders (id, placed) SETTINGS enable_extended_results_for_datetime_functions = 1 VALUES (54, toStartOfYear({d:Date32}))
