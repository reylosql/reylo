select json_object('count(', title) as o, count(*) as n from posts group by o with rollup
