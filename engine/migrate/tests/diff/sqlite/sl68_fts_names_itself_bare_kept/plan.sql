create table reylo_aside_notes as select rowid, body from notes;
drop table notes;
create table notes (body text not null check ("notes"."body" <> ''));
insert into notes (rowid, body) select rowid, body from reylo_aside_notes;
drop table reylo_aside_notes;
insert into notes_fts (notes_fts) values ('rebuild');
