insert into kv (k, v) values (:k, :v) on conflict (k) do select where kv.v is not null returning k, v
