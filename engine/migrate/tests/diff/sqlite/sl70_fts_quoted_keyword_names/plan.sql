create table "new_My Notes" (Id integer primary key, "Body Text" text);
insert into "new_My Notes" ("Body Text") select "Body Text" from "My Notes";
drop table "My Notes";
alter table "new_My Notes" rename to "My Notes";
insert into "order" ("order") values ('rebuild');
