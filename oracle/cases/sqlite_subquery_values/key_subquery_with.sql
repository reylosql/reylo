with input as (select :id as id) select name from users where id = (select id from input)
