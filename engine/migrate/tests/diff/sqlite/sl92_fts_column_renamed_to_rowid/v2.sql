create table notes (rowid integer, body text, check (body <> ''));
create virtual table notes_fts using fts5(body, content='notes');
