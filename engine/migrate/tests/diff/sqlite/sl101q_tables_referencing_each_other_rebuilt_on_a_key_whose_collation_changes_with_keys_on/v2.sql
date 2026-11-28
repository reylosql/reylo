create table zz_a (id int, y text collate nocase check (y collate nocase <> ''), unique (y collate nocase));
create table m (id integer primary key, zid integer references z (id), y int unique);
create table z (id integer primary key, mid integer references m (id), x int unique, aid text references zz_a (y));
