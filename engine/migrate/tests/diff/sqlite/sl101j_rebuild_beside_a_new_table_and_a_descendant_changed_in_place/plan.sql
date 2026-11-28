-- loses data: a.w
create table n (id integer primary key, aid integer references a (id));
create table new_t (id integer primary key, v int unique, nid integer references n (id));
create table new_p (id integer primary key, tid integer references new_t (id));
create table new_a (id integer primary key, pid integer references new_p (id), w int);
insert into new_t (id, v) select id, v from t;
insert into new_p (id, tid) select id, tid from p;
insert into new_a (id, pid, w) select id, pid, w from a;
drop table a;
drop table p;
drop table t;
alter table new_t rename to t;
alter table new_p rename to p;
alter table new_a rename to a;
alter table a drop column w;
