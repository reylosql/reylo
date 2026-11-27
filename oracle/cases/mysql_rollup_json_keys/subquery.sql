select (select json_object('k', json_object('t', p.title)) from posts p group by json_object('t', p.title) with rollup order by json_object('t', p.title) limit 1) as o
