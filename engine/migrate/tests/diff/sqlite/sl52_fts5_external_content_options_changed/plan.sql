drop trigger notes_ad;
drop trigger notes_ai;
drop table notes_fts;
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id', tokenize='porter');
insert into notes_fts (notes_fts) values ('rebuild');
create trigger notes_ad after delete on notes begin insert into notes_fts (notes_fts, rowid, body) values ('delete', old.id, old.body); end;
create trigger notes_ai after insert on notes begin insert into notes_fts (rowid, body) values (new.id, new.body); end;
