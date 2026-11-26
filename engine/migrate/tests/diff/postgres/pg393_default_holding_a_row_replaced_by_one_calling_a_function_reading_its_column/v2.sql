create table t (id integer primary key, n bigint);
create table h (id integer primary key, n bigint);
create function g(r t) returns bigint language sql stable return (select coalesce(max(n), 0) from h) + r.id;
alter table h alter column n set default g(row(0, 0)::t);
