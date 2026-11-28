-- loses data: a.w
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, bid integer references new_b (id));
create table new_c (id integer primary key, aid int references new_a (id));
create table new_b (id integer primary key, cid integer references new_c (id));
insert into new_a (id, bid) select id, bid from a;
insert into new_c (id, aid) select id, aid from c;
insert into new_b (id, cid) select id, cid from b;
drop table b;
drop table c;
drop table a;
alter table new_a rename to a;
alter table new_c rename to c;
alter table new_b rename to b;
