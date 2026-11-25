alter table child not of;
alter table child add column b integer;
alter table child add column a integer generated always as ((b + 1)) stored;
alter table child inherit parent;
drop type ty;
