create table notes (body text);
create table tags (note_id integer, tag text);
create virtual table notes_fts using fts5(body, content='notes');
create trigger tags_ai after insert on tags begin insert into notes_fts (notes_fts, rowid, body) select 'delete', rowid, body from notes where rowid = new.note_id; insert into notes_fts (rowid, body) select rowid, body from notes where rowid = new.note_id; end;
