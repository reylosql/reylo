-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_z (id integer primary key, bid integer references new_b (id), x int unique);
create table new_a (id integer primary key, zid integer references new_z (id));
create table new_b (id integer primary key, aid integer references new_a (id));
insert into new_z (id, bid, x) select id, bid, x from z;
insert into new_a (id, zid) select id, zid from a;
insert into new_b (id, aid) select id, aid from b;
drop table b;
drop table a;
drop table z;
alter table new_z rename to z;
alter table new_a rename to a;
alter table new_b rename to b;
alter table a add column extra text;
