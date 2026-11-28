create table t (id integer primary key, v int unique, nid integer references n (id));
create table p (id integer primary key, tid integer references t (id));
create table a (id integer primary key, pid integer references p (id));
create table n (id integer primary key, aid integer references a (id));
