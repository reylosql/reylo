create table a (id integer primary key, bid integer references b (id));
create table b (id integer primary key, aid integer references a (id));
