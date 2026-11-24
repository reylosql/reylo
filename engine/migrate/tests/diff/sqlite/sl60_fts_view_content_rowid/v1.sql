create table notes (body text);
create view notes_v as select rowid as id, body from notes;
create virtual table notes_fts using fts5(body, content='notes_v', content_rowid='id');
