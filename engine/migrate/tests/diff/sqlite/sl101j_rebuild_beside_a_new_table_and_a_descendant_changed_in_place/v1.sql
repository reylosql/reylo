create table t (id integer primary key, v int);
create table p (id integer primary key, tid integer references t (id));
create table a (id integer primary key, pid integer references p (id), w int);
