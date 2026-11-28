select json_object('plain', plain) as doc
from wide_sets
where exists (select 1 from dual)
