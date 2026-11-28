select coalesce(year(d), ti) as v, if(id > 1, ti, year(d)) as i, :n as n from t
