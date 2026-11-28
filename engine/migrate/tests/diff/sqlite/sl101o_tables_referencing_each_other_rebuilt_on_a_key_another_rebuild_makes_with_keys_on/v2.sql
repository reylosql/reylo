create table a (id integer primary key, my int references m (id), y int unique);
create table m (id integer primary key, zid integer references z (id), y int unique);
create table z (id integer primary key, mid integer references m (id), x int unique, aid int references a (y));
