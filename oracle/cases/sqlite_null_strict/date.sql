-- A date function is NULL for a NULL time value.
select u.name, u.born from posts p left join users u on u.id = p.user_id + 1000 where date(u.born) is not null
