create table notes (id integer primary key desc, body text);
create virtual table notes_fts using fts5(body, content='notes');
