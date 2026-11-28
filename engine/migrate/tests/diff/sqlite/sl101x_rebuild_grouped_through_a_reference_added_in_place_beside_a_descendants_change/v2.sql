create table l (id integer primary key, mid integer references m (id), v int unique);
create table m (id integer primary key, lid integer references l (id));
create table c (id integer primary key, mid integer references m (id));
