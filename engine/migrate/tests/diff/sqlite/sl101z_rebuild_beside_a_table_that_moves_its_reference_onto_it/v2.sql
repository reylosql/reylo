create table l (id integer primary key, v int unique);
create table m (id integer primary key, lid integer references l (id));
create table y (id integer primary key, mid int, lref int references l (id));
