create table a (id integer primary key, bid integer references b (id), w int unique);
create table b (id integer primary key, cid integer references c (id));
create table c (id integer primary key, aid int);
