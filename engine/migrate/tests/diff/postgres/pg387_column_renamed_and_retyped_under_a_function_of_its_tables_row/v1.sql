create table t (id integer primary key, n integer);
create function label(x t) returns text language sql stable return x.id::text;
