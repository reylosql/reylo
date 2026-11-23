create table notes (id integer primary key autoincrement, body text);
create virtual table notes_fts using fts4(body, content=notes);
