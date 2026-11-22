create type mood as enum ('a');
create table x (c mood);
create view v as select c from x;
create table z (i integer, backup v);
create table y (i integer, c mood default 'a');
