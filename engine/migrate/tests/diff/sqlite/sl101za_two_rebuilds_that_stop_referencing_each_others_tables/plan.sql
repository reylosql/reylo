-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_l (id integer primary key, wid integer);
create table new_m (id integer primary key, lid integer references new_l (id));
create table new_y (id integer primary key, mid integer);
create table new_w (id integer primary key, yid integer references new_y (id));
insert into new_l (id, wid) select id, wid from l;
insert into new_m (id, lid) select id, lid from m;
insert into new_y (id, mid) select id, mid from y;
insert into new_w (id, yid) select id, yid from w;
drop table w;
drop table y;
drop table m;
drop table l;
alter table new_l rename to l;
alter table new_m rename to m;
alter table new_y rename to y;
alter table new_w rename to w;
