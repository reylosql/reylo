create table a (id integer primary key, zid integer references z (id));
create table b (id integer primary key, aid integer references a (id));
create table z (id integer primary key, bid integer references b (id), x int);
