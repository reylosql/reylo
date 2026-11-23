select array(select unnest(array[now()])::text order by generate_series(1, 3)) as a
