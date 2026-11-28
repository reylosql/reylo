create table p (id integer primary key, code int unique);
create table c (id integer primary key, code int references p (code));
