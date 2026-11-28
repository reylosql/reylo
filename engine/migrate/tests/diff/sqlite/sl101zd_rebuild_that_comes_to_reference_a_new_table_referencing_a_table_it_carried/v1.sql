create table a (id integer primary key, fid integer references f (id));
create table f (id integer primary key, v int);
