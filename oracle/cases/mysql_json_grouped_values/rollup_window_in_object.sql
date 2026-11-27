select user_id, json_object('w', json_arrayagg(user_id) over ()) as o from posts group by user_id with rollup
