-- concat_ws skips a NULL after its separator, so the null extension passes this WHERE.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where concat_ws(',', u.email) = ''
