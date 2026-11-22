select array[concat(m.nickname)] as a from members m group by rollup (concat(nickname))
