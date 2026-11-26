create table z (id integer, n bigint);
create table a (id integer, y bigint default (row(0, 0)::z).id);
