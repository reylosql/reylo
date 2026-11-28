select u.id from users u join (select id, user_id, json_object('k', id) as card from posts) p on u.id = p.user_id where p.id = :id
