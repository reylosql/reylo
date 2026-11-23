drop trigger tags_ai;
drop table notes_fts;
create table new_notes (id integer primary key, body text);
insert into new_notes (body) select body from notes;
drop table notes;
alter table new_notes rename to notes;
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id', tokenize='porter');
insert into notes_fts (notes_fts) values ('rebuild');
create trigger tags_ai after insert on tags begin insert into notes_fts (notes_fts, rowid, body) select 'delete', id, body from notes where id = new.note_id; insert into notes_fts (rowid, body) select id, body from notes where id = new.note_id; end;
