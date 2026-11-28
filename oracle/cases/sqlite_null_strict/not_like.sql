-- NOT LIKE is NULL for a NULL operand.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where not (u.email like 'a%')
