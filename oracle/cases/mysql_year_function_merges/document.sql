select json_object('v', coalesce(year(d), ti)) as o, :n as n from t
