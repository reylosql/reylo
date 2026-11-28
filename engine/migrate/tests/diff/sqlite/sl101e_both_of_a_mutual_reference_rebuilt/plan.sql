-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, zid integer references new_z (id), y int unique);
create table new_z (id integer primary key, aid integer references new_a (id), x int unique);
insert into new_a (id, zid, y) select id, zid, y from a;
insert into new_z (id, aid, x) select id, aid, x from z;
drop table z;
drop table a;
alter table new_a rename to a;
alter table new_z rename to z;
