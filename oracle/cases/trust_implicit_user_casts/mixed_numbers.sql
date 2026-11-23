-- coalesce of numeric and int4 is numeric, as Postgres resolves it, so round over it is the built-in.
select round(coalesce(n.maybe, 0), 2) as rounded, greatest(n.qty, n.amount) as widest from numbers n
