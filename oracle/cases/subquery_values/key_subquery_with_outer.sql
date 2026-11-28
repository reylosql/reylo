with input as (select :id::int as uid) select name from users where id = (select id from input)
