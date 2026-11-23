create table notes (note_id integer primary key, body text not null);
create virtual table notes_fts using fts5(body, content=notes);
