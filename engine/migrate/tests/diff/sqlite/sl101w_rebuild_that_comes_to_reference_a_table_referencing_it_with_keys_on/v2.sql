create table p (id integer primary key, x int unique, kid integer references k (id));
create table k (id integer primary key, pid integer references p (id));
