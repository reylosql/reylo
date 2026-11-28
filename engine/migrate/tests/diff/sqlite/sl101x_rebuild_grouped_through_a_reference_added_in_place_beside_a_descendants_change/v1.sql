create table l (id integer primary key, mid integer references m (id), v int);
create table m (id integer primary key);
create table c (id integer primary key, mid integer references m (id), w int);
