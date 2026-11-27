select p.title, json_object('t', p.title, 'n', count(*)) as o from posts p group by 1 with rollup
