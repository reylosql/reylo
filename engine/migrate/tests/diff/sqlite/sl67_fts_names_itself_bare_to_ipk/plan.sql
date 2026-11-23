create table reylo_aside_notes as select body from notes;
drop table notes;
create table notes (id integer primary key, body text check ("notes"."body" <> ''));
insert into notes (body) select body from reylo_aside_notes;
drop table reylo_aside_notes;
insert into notes_fts (notes_fts) values ('rebuild');
