select (select json_object('k', json_object('t', title)) from titled group by titled.k with rollup order by titled.k limit 1) as o
