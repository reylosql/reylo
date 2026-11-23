drop view notes_v;
create table new_notes (id integer primary key, body text);
insert into new_notes (body) select body from notes;
drop table notes;
alter table new_notes rename to notes;
create view notes_v as select rowid as id, body from notes;
