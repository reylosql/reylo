-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_zz_a (id int, y text collate nocase check (y collate nocase <> ''), unique (y collate nocase));
create table new_z (id integer primary key, mid integer references new_m (id), x int, aid text);
create table new_m (id integer primary key, zid integer references new_z (id), y int);
insert into new_zz_a (rowid, id, y) select rowid, id, y from zz_a;
insert into new_z (id, mid, x, aid) select id, mid, x, aid from z;
insert into new_m (id, zid, y) select id, zid, y from m;
drop table m;
drop table z;
drop table zz_a;
alter table new_zz_a rename to zz_a;
alter table new_z rename to z;
alter table new_m rename to m;
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_m (id integer primary key, zid integer references new_z (id), y int unique);
create table new_z (id integer primary key, mid integer references new_m (id), x int unique, aid text references zz_a (y));
insert into new_m (id, zid, y) select id, zid, y from m;
insert into new_z (id, mid, x, aid) select id, mid, x, aid from z;
drop table z;
drop table m;
alter table new_m rename to m;
alter table new_z rename to z;
