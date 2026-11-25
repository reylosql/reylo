create table a (new integer, check (a.new > 0)) strict;
create table b (id integer primary key, new integer check (main.b.new > 0)) strict;
create table c (c integer, check (c.c > 0));
create table d (id integer primary key, new integer unique, p integer references d (new));
create table e (new integer, check ('e'.new > 0));
