create table b (id integer primary key, q integer references c (id), label text);
create table c (id integer primary key, x int);
