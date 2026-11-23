drop table notes_fts;
drop view notes_v;
create view notes_v as select id, upper(body) || ' x' as body from notes;
create virtual table notes_fts using fts5(body, content='notes_v', content_rowid='id');
insert into notes_fts (notes_fts) values ('rebuild');
