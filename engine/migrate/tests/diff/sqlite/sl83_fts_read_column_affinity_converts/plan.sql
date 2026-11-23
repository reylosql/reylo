-- loses data: notes.body changes from text to integer, which stores text that reads as a number as that number ('007' becomes 7)
create table new_notes (id integer primary key, body integer);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
insert into notes_fts (notes_fts) values ('rebuild');
