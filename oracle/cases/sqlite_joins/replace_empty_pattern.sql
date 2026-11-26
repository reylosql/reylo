-- `replace(X, '', Z)` is X whatever Z is, so the null extension passes this WHERE.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where replace('x', '', u.email) = 'x'
