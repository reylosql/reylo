create table a (id integer primary key, fid integer);
create table f (id integer primary key, v int unique, nid integer references n (id));
create table n (id integer primary key, aid integer references a (id));
