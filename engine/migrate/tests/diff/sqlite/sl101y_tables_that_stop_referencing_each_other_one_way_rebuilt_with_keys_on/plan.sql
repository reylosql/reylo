-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_b (id integer primary key, aid integer);
create table new_a (id integer primary key, bid integer references new_b (id));
insert into new_b (id, aid) select id, aid from b;
insert into new_a (id, bid) select id, bid from a;
drop table a;
drop table b;
alter table new_b rename to b;
alter table new_a rename to a;
