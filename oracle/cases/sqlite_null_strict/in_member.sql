-- IN is true where a member that is no NULL matches, so the null extension passes this WHERE.
select u.name, u.score from posts p left join users u on u.id = p.user_id + 1000 where 1 in (u.score, 1)
