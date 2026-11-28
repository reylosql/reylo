create table a (id integer primary key, cid int references c (id));
create table c (id integer primary key, aid int references a (id));
