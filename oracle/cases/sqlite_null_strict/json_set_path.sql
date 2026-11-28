-- json_set takes a NULL path as no edit, so the null extension passes this WHERE.
select u.name, u.email from posts p left join users u on u.id = p.user_id + 1000 where json_set('{}', u.email, 1) = '{}'
