with input as (select 1 as id) select u.id from users u where u.id = (with value as (select id as k) select value.k from input, value)
