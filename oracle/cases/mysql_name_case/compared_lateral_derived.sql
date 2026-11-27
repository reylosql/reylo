select l.x from E A, lateral (select d.x from (select A.x as x from e a) d where d.x = :v) l
