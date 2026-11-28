with w as (select coalesce(y, ti) as v from x) select v, :n as n from w
