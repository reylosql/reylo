create table a (id integer primary key, zid integer references z (id));
create table z (id integer primary key, aid integer references a (id), x int);
