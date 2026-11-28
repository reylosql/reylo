select v, :n as n from (select id, coalesce(y, ti) as v from x) d
