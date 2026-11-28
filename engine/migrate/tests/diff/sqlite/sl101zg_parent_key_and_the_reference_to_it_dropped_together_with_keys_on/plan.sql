create table new_p (id integer primary key, code int);
create table new_c (id integer primary key, code int);
insert into new_p (id, code) select id, code from p;
insert into new_c (id, code) select id, code from c;
drop table c;
drop table p;
alter table new_p rename to p;
alter table new_c rename to c;
