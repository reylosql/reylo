-- md5 here is the stored function, which gives a value for NULL, so the test keeps the rows the
-- outer join null-extends.
select u.name from posts p left join users u on u.id = p.user_id where md5(u.email) is not null
