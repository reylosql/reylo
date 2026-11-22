-- Under a GROUP BY every group has a row, so an aggregate of a non-null column is non-null.
select n.k, round(coalesce(sum(n.amount), 0), 2) as rounded, round(sum(n.qty), 0) as rounded_int_sum, abs(avg(n.qty)) as average
from numbers n
group by n.k
