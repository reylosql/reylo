select l.c from t A, lateral (select a.c from t b left join t a on a.id = b.id + 100 where A.c is not null) l
