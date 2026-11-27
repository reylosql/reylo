with C as (select id, x from E), c as (select id, x from e) select C.id from C where C.x = :v
