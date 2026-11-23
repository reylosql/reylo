create table notes (id integer primary key, body text);
create virtual table notes_fts using fts4(body, content=notes);
