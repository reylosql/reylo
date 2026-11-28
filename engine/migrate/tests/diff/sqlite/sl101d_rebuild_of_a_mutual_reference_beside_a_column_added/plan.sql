-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_z (id integer primary key, aid integer references new_a (id), x int unique);
create table new_a (id integer primary key, zid integer references new_z (id));
insert into new_z (id, aid, x) select id, aid, x from z;
insert into new_a (id, zid) select id, zid from a;
drop table a;
drop table z;
alter table new_z rename to z;
alter table new_a rename to a;
alter table a add column extra text;
