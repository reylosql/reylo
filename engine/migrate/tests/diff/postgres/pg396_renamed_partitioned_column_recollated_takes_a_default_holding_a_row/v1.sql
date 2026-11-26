create table z (id integer, n integer);
create table a (id integer, x text collate "C") partition by list (id);
create table a1 partition of a for values in (1);
