create table notes (id integer primary key, body text);
create table keep (id integer primary key);
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');
