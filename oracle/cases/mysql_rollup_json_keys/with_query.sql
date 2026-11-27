with d as (select title, json_object('t', title) as k from posts) select json_object('t', d.title) as o, count(*) as n from d group by d.k with rollup
