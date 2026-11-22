select array[concat(g.x)] as k from grouped g group by rollup (concat(g.x), g.k)
