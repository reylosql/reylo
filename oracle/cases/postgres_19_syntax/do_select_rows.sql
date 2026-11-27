insert into kv (k, v) values (:a, 'a'), (:b, 'b') on conflict (k) do select returning k
