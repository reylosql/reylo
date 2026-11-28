-- -> and ->> are NULL for a NULL operand.
select u.name, u.doc from posts p left join users u on u.id = p.user_id + 1000 where u.doc ->> '$' is not null
