select v, :n as n from (select coalesce(year(d), ti) as v from t) x
