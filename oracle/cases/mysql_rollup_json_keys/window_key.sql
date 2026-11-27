select json_arrayagg(json_object('t', title)) over () as w from posts group by json_object('t', title) with rollup
