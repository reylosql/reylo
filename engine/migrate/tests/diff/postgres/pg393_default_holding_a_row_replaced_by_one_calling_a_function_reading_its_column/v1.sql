create table t (id integer primary key, n integer);
create table h (id integer primary key, n integer default (row(0, 0)::t).id);
