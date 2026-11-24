drop table notes_fts;
create table new_notes (id integer primary key, body text not null);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
-- notes.id is renamed note_id, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table notes rename column id to note_id;
create virtual table notes_fts using fts5(body, content='notes', content_rowid='note_id');
insert into notes_fts (notes_fts) values ('rebuild');
