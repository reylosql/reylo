-- Behind a pattern that is a non-empty text literal, a NULL replacement makes `replace` NULL, so this WHERE rejects the rows where `u.email` is NULL.
select u.name, u.email from posts p left join users u on u.id = p.user_id where replace('x', 'a', u.email) = 'x'
