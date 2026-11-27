select (select coalesce(json_arrayagg(u.id), json_array()) from users u) as o, count(*) as n from posts p group by o with rollup
