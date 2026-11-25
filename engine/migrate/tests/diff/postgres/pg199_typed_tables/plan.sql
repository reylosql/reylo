-- file 1 of 3
alter table t of ty;
alter type ty2 add attribute y text cascade;
create table n of ty2;
alter table u alter column a set default 1;
alter table u alter column a set not null;
alter table w alter column y set default 'y'::text;
-- file 2 of 3
create unique index concurrently u_pkey on u (a);
-- file 3 of 3
alter table u add constraint u_pkey primary key using index u_pkey;
