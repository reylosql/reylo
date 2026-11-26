create table new_t (old text, status text default 'new');
insert into new_t (rowid, old, status) select rowid, old, status from t;
drop table t;
alter table new_t rename to t;
-- t.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table t rename column old to new;
