create table a (id integer primary key, bid int references b (id));
create table b (id integer primary key, aid int references a (id));
create table c (id integer primary key, bid int, aid int references a (id));
