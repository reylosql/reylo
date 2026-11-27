select e.key, e.value from jsonb_each_text('{"a": 1}') e
