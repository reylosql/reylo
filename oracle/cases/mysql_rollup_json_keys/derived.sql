select json_object('t', d.title) as o, count(*) as n from (select title, json_object('t', title) as k from posts) d group by d.k with rollup
