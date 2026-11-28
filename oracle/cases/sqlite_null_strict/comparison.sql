-- A comparison inside another is NULL for a NULL operand.
select u.name, u.score from posts p left join users u on u.id = p.user_id + 1000 where (u.score > 0) = 1
