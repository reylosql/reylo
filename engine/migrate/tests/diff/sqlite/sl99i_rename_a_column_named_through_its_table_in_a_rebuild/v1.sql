create table a (old integer, check (a.old > 0));
create table b (id integer primary key, old integer check (main.b.old > 0));
create table c (old integer, check (c.old > 0));
create table d (id integer primary key, old integer unique, p integer references d (old));
create table e (old integer, check ('e'.old > 0));
