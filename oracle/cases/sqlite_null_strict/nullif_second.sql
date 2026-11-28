-- nullif(a, NULL) is a, so the null extension passes this WHERE.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where nullif('x', u.email) is not null
