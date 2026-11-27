-- A strict built-in over the stored function is NULL only where the function is.
select u.name from posts p left join users u on u.id = p.user_id where length(sha(u.email)) > 0
