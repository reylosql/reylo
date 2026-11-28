-- loses data: c.w
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_l (id integer primary key, mid integer references new_m (id), v int unique);
create table new_m (id integer primary key);
create table new_c (id integer primary key, mid integer references new_m (id), w int);
insert into new_l (id, mid, v) select id, mid, v from l;
insert into new_m (id) select id from m;
insert into new_c (id, mid, w) select id, mid, w from c;
drop table c;
drop table m;
drop table l;
alter table new_l rename to l;
alter table new_m rename to m;
alter table new_c rename to c;
alter table c drop column w;
alter table m add column lid integer references l (id);
