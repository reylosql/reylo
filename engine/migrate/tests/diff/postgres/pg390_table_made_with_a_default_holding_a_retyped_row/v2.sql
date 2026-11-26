create table t (id integer primary key, n bigint);
create table u (id integer primary key, x text default (row(0, 0)::t)::text);
