drop table docs;
create virtual table docs using fts5(body, content='other', content_rowid='id');
insert into docs (docs) values ('rebuild');
