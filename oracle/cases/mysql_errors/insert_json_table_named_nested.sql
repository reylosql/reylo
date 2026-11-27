insert into posts (id, user_id, title) select u.id, u.id, j.nested from users u, json_table(json_array(u.email), '$[*]' columns (nested varchar(4) path '$' error on error)) j
