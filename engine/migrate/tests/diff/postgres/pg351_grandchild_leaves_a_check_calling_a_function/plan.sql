alter table grandparent drop constraint ck;
alter table leaf no inherit middle;
alter table leaf drop constraint if exists ck;
drop function parent_check(integer);
