create table reylo_aside_a as select rowid, old from a;
drop table a;
create table a (old integer, check (a.old > 0)) strict;
insert into a (rowid, old) select rowid, old from reylo_aside_a;
drop table reylo_aside_a;
create table reylo_aside_b as select id, old from b;
drop table b;
create table b (id integer primary key, old integer check (main.b.old > 0)) strict;
insert into b (id, old) select id, old from reylo_aside_b;
drop table reylo_aside_b;
-- a.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table a rename column old to new;
-- b.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table b rename column old to new;
-- c.old is renamed c, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table c rename column old to c;
-- d.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table d rename column old to new;
-- e.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table e rename column old to new;
