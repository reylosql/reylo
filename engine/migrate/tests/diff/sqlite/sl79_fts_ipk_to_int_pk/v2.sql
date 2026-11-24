create table notes (id int primary key, body text);
create virtual table notes_fts using fts5(body, content='notes');
