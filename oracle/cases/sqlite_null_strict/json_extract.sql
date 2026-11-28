-- json_extract is NULL for a NULL document.
select u.name, u.doc from posts p left join users u on u.id = p.user_id + 1000 where json_extract(u.doc, '$.a') is not null
