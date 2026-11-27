select (select json_object('k', json_object('t', p.title))) as o, count(*) as n from posts p group by json_object('t', p.title) with rollup
