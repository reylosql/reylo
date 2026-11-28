-- loses data: z.w
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_b (id integer primary key, zid int references new_z (id));
create table new_z (id integer primary key, bid int references new_b (id));
insert into new_b (id, zid) select id, zid from b;
insert into new_z (id, bid) select id, bid from z;
drop table z;
drop table b;
alter table new_b rename to b;
alter table new_z rename to z;
