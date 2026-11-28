-- BETWEEN is never true for a NULL bound.
select u.name, u.score from posts p left join users u on u.id = p.user_id + 1000 where 0 between u.score and 1e308
