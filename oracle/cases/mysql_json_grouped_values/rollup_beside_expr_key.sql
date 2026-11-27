select p.user_id + 0 as k, json_object('n', count(*)) as o from posts p group by k with rollup
