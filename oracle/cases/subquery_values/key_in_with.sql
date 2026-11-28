with input as (select :id::int as id) select name from users where id in (select id from input)
