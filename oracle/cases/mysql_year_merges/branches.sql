select if(id > 0, y, ti) as i, case when id > 0 then ti else y end as c, :n as n from x
