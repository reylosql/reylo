create table l (id integer primary key, wid integer references w (id));
create table m (id integer primary key, lid integer references l (id));
create table y (id integer primary key, mid integer references m (id));
create table w (id integer primary key, yid integer references y (id));
