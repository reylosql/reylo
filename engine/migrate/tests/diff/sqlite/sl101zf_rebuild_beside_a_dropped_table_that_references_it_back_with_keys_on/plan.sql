-- loses data: x
create table new_p (id integer primary key, xid int);
insert into new_p (id, xid) select id, xid from p;
drop table p;
alter table new_p rename to p;
drop table x;
