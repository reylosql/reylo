-- nullif is NULL for a NULL first argument.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where nullif(u.email, 'x') is not null
