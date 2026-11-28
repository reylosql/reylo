select json_object('c', coalesce(ti, ti), 'm', if(id > 1, ti, -1)) as o, :n as n from t
