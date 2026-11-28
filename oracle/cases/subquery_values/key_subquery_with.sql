with input as (select :id::int as id) select name from users where id = (select id from input)
