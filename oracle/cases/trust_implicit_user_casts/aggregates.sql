-- With no GROUP BY each aggregate may see no rows; coalesce's fallback keeps the round non-null.
select
  round(coalesce(sum(n.amount), 0), 2) as rounded_sum,
  round(sum(n.amount), 2) as rounded_bare_sum,
  abs(coalesce(avg(n.qty), 0)) as abs_avg,
  round(coalesce(max(n.amount), 0), 2) as rounded_max
from numbers n
