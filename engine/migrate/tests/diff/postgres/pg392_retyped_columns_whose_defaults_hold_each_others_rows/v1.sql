create table a (id integer, n integer);
create table b (id integer, n integer default (row(0, 0)::a).id);
alter table a alter column n set default (row(0, 0)::b).id;
