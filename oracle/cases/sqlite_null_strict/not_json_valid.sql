-- json_valid of NULL is 0 before SQLite 3.42, so NOT of it is true there, and the column stays nullable.
select u.name, u.doc from posts p left join users u on u.id = p.user_id + 1000 where not json_valid(u.doc)
