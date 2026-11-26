create table a (id integer, n bigint);
create table b (id integer, n bigint default (row(0, 0)::a).id);
alter table a alter column n set default (row(0, 0)::b).id;
