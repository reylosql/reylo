select json_object('t', title) as o, count(*) as n from titled group by k with rollup
