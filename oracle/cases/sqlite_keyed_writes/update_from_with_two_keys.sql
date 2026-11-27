with input as (select :id as id, :name as name) update users set name = input.name from input where users.id = input.id returning users.id
