drop view hits;
drop table notes_fts;
create table new_notes (id integer primary key, body text not null);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id', tokenize='porter');
create view hits as select n.id, n.body, f.rank from notes n join notes_fts f on f.rowid = n.id;
insert into notes_fts (notes_fts) values ('rebuild');
