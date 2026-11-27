select json_object('k', json_object('t', title)) as o, count(*) as n from posts group by json_object('t', title) with rollup
