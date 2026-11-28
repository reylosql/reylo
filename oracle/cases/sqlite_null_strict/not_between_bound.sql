-- NOT BETWEEN is true for a NULL bound where the other bound settles it, so the null extension passes this WHERE.
select u.name, u.score from posts p left join users u on u.id = p.user_id + 1000 where not (5 between u.score and 1)
