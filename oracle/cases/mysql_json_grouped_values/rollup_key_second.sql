select p.user_id, json_object('t', p.title) as o, count(*) as n from posts p group by p.user_id, o with rollup
