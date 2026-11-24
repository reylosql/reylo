UPDATE orders SET placed = toStartOfYear({d:Date32}) WHERE id = 1 SETTINGS enable_extended_results_for_datetime_functions = 1
