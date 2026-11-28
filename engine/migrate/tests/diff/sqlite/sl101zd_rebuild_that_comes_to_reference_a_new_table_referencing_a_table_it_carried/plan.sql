create table n (id integer primary key, aid integer references a (id));
create table new_f (id integer primary key, v int unique, nid integer references n (id));
create table new_a (id integer primary key, fid integer);
insert into new_f (id, v) select id, v from f;
insert into new_a (id, fid) select id, fid from a;
drop table a;
drop table f;
alter table new_f rename to f;
alter table new_a rename to a;
