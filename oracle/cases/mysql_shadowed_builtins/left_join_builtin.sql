-- A built-in the server still has is NULL for NULL, and the join is inner.
select u.name from posts p left join users u on u.id = p.user_id where lower(u.email) is not null
