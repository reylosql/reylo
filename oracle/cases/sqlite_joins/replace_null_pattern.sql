-- A NULL pattern makes `replace` NULL, so this WHERE rejects the null extension.
select u.name, u.email from posts p left join users u on u.id = p.user_id where replace('x', u.email, 'y') = 'x'
