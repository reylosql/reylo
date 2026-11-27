select json_object('k', json_object('t', title)) as o, json_object('t', title) as 'x\0suffix', count(*) as n from posts group by x with rollup
