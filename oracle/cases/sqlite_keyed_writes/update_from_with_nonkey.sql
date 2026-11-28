with input as (select :id as id) update users set name = 'x' from input where users.account_id = input.id returning users.id
