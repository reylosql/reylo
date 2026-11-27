select json_object('t', p.title) as o, count(*) as n from posts p group by JSON_OBJECT( 't', title ) with rollup
