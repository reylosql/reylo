drop table notes_fts;
create virtual table notes_fts using fts5(title, body, content="notes", content_rowid=ref);
insert into notes_fts (notes_fts) values ('rebuild');
