create table t (id integer primary key, m bigint);
create function label(x t) returns text language sql stable return x.id::text;
