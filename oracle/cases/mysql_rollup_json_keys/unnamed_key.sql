select json_object('k', json_object('t', title)) as o, json_object('t', title), count(*) as n from posts group by `json_object('t', title)` with rollup
