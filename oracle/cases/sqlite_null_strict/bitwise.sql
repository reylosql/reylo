-- A bitwise operator is NULL for a NULL operand, and a bare value rejects NULL.
select u.name, u.flags from posts p left join users u on u.id = p.user_id + 1000 where u.flags & 1
