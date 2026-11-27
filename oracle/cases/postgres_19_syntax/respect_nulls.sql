select first_value(v) respect nulls over (order by k) as v from kv
