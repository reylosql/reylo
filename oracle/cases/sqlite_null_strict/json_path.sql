-- json_extract is NULL for a NULL path.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where json_extract('{"a":1}', u.email) is not null
