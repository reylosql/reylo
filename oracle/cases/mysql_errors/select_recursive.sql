with recursive r (x, k) as (select email, 1 from users union all select concat(x, x), k + 1 from r where k < 3) select x from r
