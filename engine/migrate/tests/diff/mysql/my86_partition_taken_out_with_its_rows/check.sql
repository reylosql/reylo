select case when (select group_concat(id order by id) from measurements) = '2,4' then 1 else (select 1 union select 2) end;
select case when (select group_concat(id order by id) from stores) = '1,3' then 1 else (select 1 union select 2) end;
