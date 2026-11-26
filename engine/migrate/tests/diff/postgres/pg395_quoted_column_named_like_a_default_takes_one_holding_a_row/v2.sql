create table a (id integer, "n set default x" bigint);
create table z (id integer, n integer, y integer default (row(0, 0)::a).id);
alter table a alter column "n set default x" set default (row(0, 0, 0)::z).id;
