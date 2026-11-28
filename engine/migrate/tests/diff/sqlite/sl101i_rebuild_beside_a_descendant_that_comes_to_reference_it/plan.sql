-- loses data: a.w
create table new_c (id integer primary key, aid int references new_a (id));
create table new_b (id integer primary key, cid integer references new_c (id));
create table new_a (id integer primary key, bid integer references new_b (id), w int unique);
insert into new_c (id, aid) select id, aid from c;
insert into new_b (id, cid) select id, cid from b;
insert into new_a (id, bid, w) select id, bid, w from a;
drop table a;
drop table b;
drop table c;
alter table new_c rename to c;
alter table new_b rename to b;
alter table new_a rename to a;
create table new_a (id integer primary key, bid integer references b (id));
insert into new_a (id, bid) select id, bid from a;
drop table a;
alter table new_a rename to a;
