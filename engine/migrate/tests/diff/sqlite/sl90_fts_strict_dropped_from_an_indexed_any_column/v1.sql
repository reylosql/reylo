create table notes (id integer primary key, body any) strict;
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');
