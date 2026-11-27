select cast(json_object('t', p.title) as json) as o, count(*) as n from posts p group by o with rollup
