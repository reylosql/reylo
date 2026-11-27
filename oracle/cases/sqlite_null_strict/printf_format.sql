-- printf is NULL for a NULL format.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where printf(u.email, 1) is not null
