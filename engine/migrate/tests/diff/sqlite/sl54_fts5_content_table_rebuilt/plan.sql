create table new_notes (id integer primary key, body text not null);
insert into new_notes (id, body) select id, body from notes;
drop table notes;
alter table new_notes rename to notes;
create trigger notes_ai after insert on notes begin insert into notes_fts (rowid, body) values (new.id, new.body); end;
create trigger notes_ad after delete on notes begin insert into notes_fts (notes_fts, rowid, body) values ('delete', old.id, old.body); end;
