create table f (id integer primary key, v int);
create table x (id integer primary key, fid integer references f (id) on delete cascade);
create table z (id integer primary key, xid integer references x (id) on delete cascade);
