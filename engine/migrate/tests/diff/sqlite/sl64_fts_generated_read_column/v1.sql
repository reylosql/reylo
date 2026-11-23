create table notes (id integer primary key, body text, search text generated always as (lower(body)) virtual);
create virtual table notes_fts using fts5(search, content='notes', content_rowid='id');
