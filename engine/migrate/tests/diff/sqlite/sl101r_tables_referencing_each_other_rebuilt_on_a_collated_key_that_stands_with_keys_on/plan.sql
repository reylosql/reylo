-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, my int references new_m (y), k text collate nocase, unique (k collate nocase));
create table new_z (id integer primary key, mid integer references new_m (id), x int unique, aid text references new_a (k));
create table new_m (id integer primary key, zid integer references new_z (id), y int unique);
insert into new_a (id, my, k) select id, my, k from a;
insert into new_z (id, mid, x, aid) select id, mid, x, aid from z;
insert into new_m (id, zid, y) select id, zid, y from m;
drop table m;
drop table z;
drop table a;
alter table new_a rename to a;
alter table new_z rename to z;
alter table new_m rename to m;
