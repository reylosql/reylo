-- A date function is NULL for a NULL modifier too.
select u.name, u.born from posts p left join users u on u.id = p.user_id + 1000 where datetime('2020-01-02', u.born) is not null
