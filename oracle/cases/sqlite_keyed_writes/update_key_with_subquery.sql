with input as (select :id as id) update users set name = 'x' where id = (select id from input) returning id
