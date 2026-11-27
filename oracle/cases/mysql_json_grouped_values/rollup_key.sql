select json_object('t', p.title) as o, count(*) as n from posts p group by o with rollup
