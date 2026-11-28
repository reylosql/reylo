create table p (id integer primary key, x int);
create table k (id integer primary key, pid integer references p (id));
