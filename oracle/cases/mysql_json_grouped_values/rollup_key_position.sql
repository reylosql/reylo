select json_object('t', p.title, 'u', p.user_id) as o, count(*) as n from posts p group by 1 with rollup
