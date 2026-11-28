create table a (id integer primary key, my int);
create table m (id integer primary key, zid integer references z (id), y int);
create table z (id integer primary key, mid integer references m (id), x int, aid int);
