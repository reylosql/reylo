create table a (id integer primary key, zid integer references z (id), y int unique);
create table z (id integer primary key, aid integer references a (id), x int unique, nid integer references zz_new (id));
create table zz_new (id integer primary key);
