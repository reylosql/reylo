create table new_a (id integer primary key, my int references m (id), y int unique);
insert into new_a (id, my, y) select id, my, y from a;
drop table a;
alter table new_a rename to a;
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_m (id integer primary key, zid integer references new_z (id), y int unique);
create table new_z (id integer primary key, mid integer references new_m (id), x int unique, aid int references a (y));
insert into new_m (id, zid, y) select id, zid, y from m;
insert into new_z (id, mid, x, aid) select id, mid, x, aid from z;
drop table z;
drop table m;
alter table new_m rename to m;
alter table new_z rename to z;
