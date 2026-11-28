-- max of two or more is NULL for a NULL argument.
select u.name, u.score from posts p left join users u on u.id = p.user_id + 1000 where max(u.score, 0) >= 0
