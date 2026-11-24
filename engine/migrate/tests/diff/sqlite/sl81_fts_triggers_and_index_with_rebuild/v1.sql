create table notes (body text);
create virtual table notes_fts using fts5(body, content='notes');
create trigger notes_ai after insert on notes begin insert into notes_fts (rowid, body) values (new.rowid, new.body); end;
create trigger notes_ad after delete on notes begin insert into notes_fts (notes_fts, rowid, body) values ('delete', old.rowid, old.body); end;
