-- unhex is NULL for NULL text.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where unhex(u.email) is not null
