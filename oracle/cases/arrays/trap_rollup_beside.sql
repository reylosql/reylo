select concat(m.nickname) as k, array[concat(m.nickname)] as a from members m group by rollup (k)
