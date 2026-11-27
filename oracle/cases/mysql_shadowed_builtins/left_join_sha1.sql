select u.name from posts p left join users u on u.id = p.user_id join users v on v.email = sha1(u.email)
