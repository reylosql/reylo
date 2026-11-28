with input as (select :id as id) insert into accounts (id, name) select input.id, 'x' from input returning id
