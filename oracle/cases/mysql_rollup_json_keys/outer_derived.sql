select (select json_object('k', json_object('t', d.title)) from posts p group by d.k with rollup order by d.k limit 1) as o from (select title, json_object('t', title) as k from posts) d
