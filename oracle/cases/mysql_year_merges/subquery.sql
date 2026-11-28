select (select coalesce(y, ti) from x where id = 2) as v, :n as n
