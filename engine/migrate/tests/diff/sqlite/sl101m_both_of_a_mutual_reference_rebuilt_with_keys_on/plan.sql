-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, zid integer references new_z (id), y int unique, note text);
create table new_z (id integer primary key, aid integer references new_a (id), x int unique, label text) strict;
insert into new_a (id, zid, y) select id, zid, y from a;
insert into new_z (id, aid, x, label) select id, aid, x, label from z;
drop table z;
drop table a;
alter table new_a rename to a;
alter table new_z rename to z;
-- z.label is renamed title, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table z rename column label to title;
