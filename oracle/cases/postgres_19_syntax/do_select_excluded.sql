insert into kv (k, v) values (:k, :v) on conflict (k) do select for share where excluded.v = kv.v returning kv.k, kv.v
