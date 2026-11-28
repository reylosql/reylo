create table l (id integer primary key, wid integer);
create table m (id integer primary key, lid integer references l (id));
create table y (id integer primary key, mid integer);
create table w (id integer primary key, yid integer references y (id));
