with input as (select :id::int as id) update users set name = 'x' where id = (select id from input) returning id
