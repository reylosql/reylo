select json_object('n', ceil(n), 'm', coalesce(n, 1.5)) as doc from t
