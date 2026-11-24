create table new_notes (id int primary key, body text);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
insert into notes_fts (notes_fts) values ('rebuild');
