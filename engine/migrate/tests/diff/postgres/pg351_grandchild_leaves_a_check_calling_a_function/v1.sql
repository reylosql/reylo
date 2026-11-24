create function parent_check(id int) returns boolean language sql immutable return id > 0;
create table grandparent (id int, constraint ck check (parent_check(id)));
create table middle () inherits (grandparent);
create table leaf (id int, constraint ck check (parent_check(id)));
alter table leaf inherit middle;
