create table b (id integer primary key, q integer references c (id), title text);
create table c (id integer primary key, x int unique);
