create table z (id integer primary key, bid int references b (id));
create table b (id integer primary key, zid int references z (id));
