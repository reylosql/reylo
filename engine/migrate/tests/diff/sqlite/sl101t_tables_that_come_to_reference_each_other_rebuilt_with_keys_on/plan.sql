-- loses data: a.w
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, cid int references new_c (id));
create table new_c (id integer primary key, aid int references new_a (id));
insert into new_a (id, cid) select id, cid from a;
insert into new_c (id, aid) select id, aid from c;
drop table c;
drop table a;
alter table new_a rename to a;
alter table new_c rename to c;
