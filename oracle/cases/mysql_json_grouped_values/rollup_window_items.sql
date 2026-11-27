select user_id, json_arrayagg(user_id) over () as w from posts group by user_id with rollup
