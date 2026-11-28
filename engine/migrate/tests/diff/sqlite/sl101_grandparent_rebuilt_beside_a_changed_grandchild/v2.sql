create table a (id integer primary key, p integer references b (id)) without rowid;
create table b (id integer primary key, q integer references c (id));
create table c (id integer primary key, x int unique);
