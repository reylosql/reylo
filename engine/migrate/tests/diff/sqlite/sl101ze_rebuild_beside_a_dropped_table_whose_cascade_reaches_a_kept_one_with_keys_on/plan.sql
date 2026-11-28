-- loses data: x
create table new_z (id integer primary key, xid integer);
insert into new_z (id, xid) select id, xid from z;
drop table z;
alter table new_z rename to z;
drop table x;
create table new_f (id integer primary key, v int unique);
insert into new_f (id, v) select id, v from f;
drop table f;
alter table new_f rename to f;
