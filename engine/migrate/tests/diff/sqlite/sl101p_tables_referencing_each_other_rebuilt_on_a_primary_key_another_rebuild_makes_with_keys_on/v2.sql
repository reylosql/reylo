create table zz_a (id int, y int primary key);
create table m (id integer primary key, zid integer references z (id), y int unique);
create table z (id integer primary key, mid integer references m (id), x int unique, aid int references zz_a);
