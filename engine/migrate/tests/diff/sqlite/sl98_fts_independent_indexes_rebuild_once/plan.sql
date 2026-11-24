create view v0 as select id, body from notes where id % 3 = 0;
create virtual table f0 using fts5(body, content='v0', content_rowid='id');
create view v1 as select id, body from notes where id % 3 = 1;
create virtual table f1 using fts5(body, content='v1', content_rowid='id');
create view v2 as select id, body from notes where id % 3 = 2;
create virtual table f2 using fts5(body, content='v2', content_rowid='id');
insert into f0 (f0) values ('rebuild');
insert into f1 (f1) values ('rebuild');
insert into f2 (f2) values ('rebuild');
