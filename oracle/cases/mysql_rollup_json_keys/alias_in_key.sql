select json_object('t', title) as o, coalesce(json_object('t', title)) as p, count(*) as n from posts group by coalesce(o) with rollup
