with input (uid) as (select :id::int) select name from users where id = (select uid from input)
