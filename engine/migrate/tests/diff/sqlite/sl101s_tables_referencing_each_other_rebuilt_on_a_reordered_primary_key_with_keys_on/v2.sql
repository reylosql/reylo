create table zz_a (id int, y int, primary key (y, id));
create table m (id integer primary key, zid integer references z (id), y int unique);
create table z (id integer primary key, mid integer references m (id), x int unique, aid int, bid int, foreign key (aid, bid) references zz_a);
