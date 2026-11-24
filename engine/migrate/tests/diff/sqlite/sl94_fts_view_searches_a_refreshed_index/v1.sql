create table notes (id integer primary key, draft text, body text generated always as ('draft ' || draft) virtual);
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');
