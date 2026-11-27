select coalesce(json_object('t', title), json_object()) as o, count(*) as n from posts group by json_object('t', title) with rollup
