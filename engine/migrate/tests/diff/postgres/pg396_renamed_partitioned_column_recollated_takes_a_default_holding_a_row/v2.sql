create table z (id integer, n bigint);
create table a (id integer, y text collate "POSIX" default (row(0, 0)::z).id::text) partition by list (id);
create table a1 partition of a for values in (1);
