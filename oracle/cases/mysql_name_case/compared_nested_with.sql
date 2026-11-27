with c as (select id, x from e) select z.id from (with C as (select id, x from E) select c.id from c where c.x = :v) z
