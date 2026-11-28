create table new_c (id integer primary key, x int unique);
create table new_b (id integer primary key, q integer references new_c (id));
create table new_a (id integer primary key, p integer references new_b (id), label text);
insert into new_c (id, x) select id, x from c;
insert into new_b (id, q) select id, q from b;
insert into new_a (id, p, label) select id, p, label from a;
drop table a;
drop table b;
drop table c;
alter table new_c rename to c;
alter table new_b rename to b;
alter table new_a rename to a;
-- a.label is renamed title, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table a rename column label to title;
