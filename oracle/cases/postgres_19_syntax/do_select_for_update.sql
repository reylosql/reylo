insert into kv (k, v) values (:k, :v) on conflict (k) do select for update returning *
