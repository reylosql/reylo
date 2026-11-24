create table notes (id integer primary key, body text);
create table tags (note_id integer, tag text);
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id', tokenize='porter');
create trigger tags_ai after insert on tags begin insert into notes_fts (notes_fts, rowid, body) select 'delete', id, body from notes where id = new.note_id; insert into notes_fts (rowid, body) select id, body from notes where id = new.note_id; end;
