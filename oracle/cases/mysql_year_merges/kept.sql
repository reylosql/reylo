select coalesce(ti, ti) as a, coalesce(y, s) as s, coalesce(y, ti) + 0 as z, ifnull(y, 0) as l, :n as n from x
