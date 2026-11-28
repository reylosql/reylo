create table p (id integer primary key, xid int references x (id) on delete cascade);
create table x (id integer primary key, pid int references p (id) on delete set null);
