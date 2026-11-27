select d.w from (select c as w from t) D left join (select c as w from t) d on d.w = D.w + 100 where D.w is not null
