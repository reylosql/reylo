select array[concat(m.nickname)] as a, array[jsonb_build_object('id', m.id)] as b from members m group by rollup (concat(m.nickname), jsonb_build_object('id', m.id))
