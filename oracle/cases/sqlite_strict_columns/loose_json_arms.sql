select case when id = 1 then json_object('n', ifnull(n, 0)) else json_object('n', id) end as j, coalesce(json_array(ifnull(n, 0)), json_array(id)) as a from loose
