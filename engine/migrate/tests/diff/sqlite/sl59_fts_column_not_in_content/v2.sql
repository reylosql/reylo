create table notes (id integer primary key, body text);
create virtual table notes_fts using fts5(body, tags unindexed, content='notes', content_rowid='id');
