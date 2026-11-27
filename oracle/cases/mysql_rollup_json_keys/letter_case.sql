select json_object('T', title) as o, count(*) as n from posts group by json_object('t', title) with rollup
