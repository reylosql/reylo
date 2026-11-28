with input as (select :id as id) insert into accounts (id, name) select input.id, 'x' from input where true on conflict (id) do update set name = 'y' returning id
