create table a (id integer primary key, code int unique);
create table b (id integer primary key, code int unique);
create table c (id integer primary key, acode int references a (code), bcode int references b (code));
