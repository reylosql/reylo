create table a (id integer primary key, bid integer references b (id), x int unique);
create table b (id integer primary key, cid integer references c (id), x int unique);
create table c (id integer primary key, bid integer references b (id), aid integer references a (id), x int unique, extra text);
