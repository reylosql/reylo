select array_agg(concat(m.nickname)) over () as a from members m group by rollup (concat(m.nickname))
