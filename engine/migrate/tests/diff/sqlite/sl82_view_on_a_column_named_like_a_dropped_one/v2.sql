create table notes (id integer primary key, body text);
create view notes_v as select rowid as id, body from notes;
