create table new_c (id integer primary key, x int unique);
create table new_b (id integer primary key, q integer references new_c (id), label text);
insert into new_c (id, x) select id, x from c;
insert into new_b (id, q, label) select id, q, label from b;
drop table b;
drop table c;
alter table new_c rename to c;
alter table new_b rename to b;
-- b.label is renamed title, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table b rename column label to title;
