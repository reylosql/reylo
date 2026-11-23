create table notes (lookup integer, body text);
create virtual table notes_fts using fts5(body, content='notes');
