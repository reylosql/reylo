with x as (insert into req (email) values ('a') returning id) insert into src (id) select id from x
