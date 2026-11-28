create table zz_a (id int, y text, unique (y collate nocase));
create table m (id integer primary key, zid integer references z (id), y int);
create table z (id integer primary key, mid integer references m (id), x int, aid text);
