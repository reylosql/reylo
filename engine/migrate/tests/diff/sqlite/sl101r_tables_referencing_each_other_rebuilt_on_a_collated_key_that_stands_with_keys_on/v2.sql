create table a (id integer primary key, my int references m (y), k text collate nocase, unique (k collate nocase));
create table m (id integer primary key, zid integer references z (id), y int unique);
create table z (id integer primary key, mid integer references m (id), x int unique, aid text references a (k));
