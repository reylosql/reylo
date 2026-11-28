-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, bid int references new_b (id));
create table new_b (id integer primary key, aid int references new_a (id));
create table new_c (id integer primary key, bid integer references new_b (id) on delete cascade, aid int);
insert into new_a (id, bid) select id, bid from a;
insert into new_b (id, aid) select id, aid from b;
insert into new_c (id, bid, aid) select id, bid, aid from c;
drop table c;
drop table b;
drop table a;
alter table new_a rename to a;
alter table new_b rename to b;
alter table new_c rename to c;
create table new_c (id integer primary key, bid int, aid int references a (id));
insert into new_c (id, bid, aid) select id, bid, aid from c;
drop table c;
alter table new_c rename to c;
