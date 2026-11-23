drop table notes_fts;
create virtual table notes_fts using fts5(body, content='archive', content_rowid='id');
insert into notes_fts (notes_fts) values ('rebuild');
