select coalesce(n, 1.5) as a, case when id > 1 then x else n end as b, max(n, x) as c, iif(id > 1, n, 2.5) as d from t
