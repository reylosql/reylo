create table new_notes (id integer primary key, draft text, body text generated always as ('published ' || draft) virtual);
insert into new_notes (id, draft) select id, draft from notes;
drop table notes;
alter table new_notes rename to notes;
create view searchable as select notes.id as rowid, notes.body from notes join notes_fts on notes_fts.rowid = notes.id where notes_fts match 'published';
create virtual table filtered_fts using fts4(body, content='searchable');
insert into notes_fts (notes_fts) values ('rebuild');
insert into filtered_fts (filtered_fts) values ('rebuild');
