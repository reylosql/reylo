create table a (id integer primary key, zid integer references z (id), y int unique, note text);
create table z (id integer primary key, aid integer references a (id), x int unique, title text) strict;
