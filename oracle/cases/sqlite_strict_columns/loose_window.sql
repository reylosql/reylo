select max(n) over () as most, lag(n) over (order by id) as before, sum(n) over (order by id) as running from loose
