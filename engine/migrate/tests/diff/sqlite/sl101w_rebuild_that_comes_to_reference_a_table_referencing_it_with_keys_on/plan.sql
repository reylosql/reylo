-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_p (id integer primary key, x int unique, kid integer references new_k (id));
create table new_k (id integer primary key, pid integer references new_p (id));
insert into new_p (id, x) select id, x from p;
insert into new_k (id, pid) select id, pid from k;
drop table k;
drop table p;
alter table new_p rename to p;
alter table new_k rename to k;
