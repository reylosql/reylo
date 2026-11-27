select k, last_value(v) ignore nulls over (order by k rows between unbounded preceding and unbounded following) as v, lag(n) ignore nulls over (order by k) as before from kv
