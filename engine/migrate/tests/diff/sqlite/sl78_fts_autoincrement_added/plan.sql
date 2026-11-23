create table new_notes (id integer primary key autoincrement, body text);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
