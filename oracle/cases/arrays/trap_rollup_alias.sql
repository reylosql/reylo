select array(select concat('x') as k group by rollup (k)) as a, array(select concat('x') group by rollup (1)) as b
