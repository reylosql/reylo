-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, bid integer references new_b (id), x int unique);
create table new_c (id integer primary key, bid integer references new_b (id), aid integer references new_a (id), x int unique, extra text);
create table new_b (id integer primary key, cid integer references new_c (id), x int unique);
insert into new_a (id, bid, x) select id, bid, x from a;
insert into new_c (id, bid, aid, x) select id, bid, aid, x from c;
insert into new_b (id, cid, x) select id, cid, x from b;
drop table b;
drop table c;
drop table a;
alter table new_a rename to a;
alter table new_c rename to c;
alter table new_b rename to b;
