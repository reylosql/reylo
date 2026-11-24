create table notes (id integer primary key, ref integer unique, title text, body text);
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');
