-- file 1 of 3
drop index x1;
alter table "x0 x" alter column c2 type text;
-- file 2 of 3
create index concurrently x1 on "x0 x" using btree (lower(c2));
-- file 3 of 3
alter index x1 alter column 1 set statistics 1000;
