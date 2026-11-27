select json_object('n', n) as o, json_array(n, n + 1) as a, (select json_group_array(n) from loose) as g from loose
