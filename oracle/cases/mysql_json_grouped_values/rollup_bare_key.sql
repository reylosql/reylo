select p.id, coalesce(json_object('id', p.id), json_object('id', 0)) as o from posts p group by id with rollup
