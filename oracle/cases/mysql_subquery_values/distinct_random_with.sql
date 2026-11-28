with volatile_ids as (select cast(floor(rand() * 3) + 1 as signed) as id from posts where posts.id = 1) select distinct x.id from users u, (select id from volatile_ids) x
