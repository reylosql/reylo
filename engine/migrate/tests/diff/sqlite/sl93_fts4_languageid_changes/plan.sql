create table new_notes (id integer primary key, body text, lid integer generated always as (2) virtual);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
insert into notes_fts (notes_fts) values ('rebuild');
