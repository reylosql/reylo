select l.id from E A, lateral (select a.id from e a where A.x = :v) l
