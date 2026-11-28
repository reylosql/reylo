with input as (select 'a' as name) select u.id from users u where u.id = (select p.user_id from input, posts p join posts q on q.title = name limit 1)
