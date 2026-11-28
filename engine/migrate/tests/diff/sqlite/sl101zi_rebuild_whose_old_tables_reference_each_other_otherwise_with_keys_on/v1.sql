create table p (id integer primary key, code int);
create table z (id integer primary key, pid int references p (id));
create table a (id integer primary key, pid int references p (id), zid int references z (id));
