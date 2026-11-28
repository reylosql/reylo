create table a (id integer primary key, bid int);
create table b (id integer primary key, aid int);
create table c (id integer primary key, bid integer references b (id) on delete cascade, aid int);
