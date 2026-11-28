-- json_extract of two paths or more is NULL for a NULL path only from SQLite 3.45, so the column stays nullable.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where json_extract('{"a":1}', '$.a', u.email) is not null
