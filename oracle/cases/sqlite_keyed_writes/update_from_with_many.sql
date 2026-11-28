with input as (select id from accounts) update users set name = 'x' from input where users.id = input.id returning users.id
