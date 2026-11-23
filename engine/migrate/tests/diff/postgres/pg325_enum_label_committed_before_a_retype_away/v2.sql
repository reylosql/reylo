create type mood as enum ('a', 'b');
create table x (c text);
create view v as select c from x;
create table z (i integer);
create table y (i integer, c mood default 'b');
