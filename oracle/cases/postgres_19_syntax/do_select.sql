insert into kv (k, v) values (:k, :v) on conflict (k) do select returning k, v, n
