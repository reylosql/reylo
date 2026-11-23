create table notes (id integer primary key, body text, title text default 'untitled');
create virtual table notes_fts using fts5(title, body, content='notes', content_rowid='id');
