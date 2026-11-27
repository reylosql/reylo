with C as (select c as w from t), c as (select c + 100 as w from t) select c.w from C left join c on c.w = C.w where C.w > 0
