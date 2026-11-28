create table l (id integer primary key, v int);
create table m (id integer primary key, lid integer references l (id));
create table y (id integer primary key, mid integer references m (id), lref int);
