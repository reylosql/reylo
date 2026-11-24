create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');
insert into notes_fts (notes_fts) values ('rebuild');
