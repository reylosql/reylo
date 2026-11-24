create table new_notes (lookup integer, body text, check (body <> ''));
insert into new_notes (_rowid_, lookup, body) select _rowid_, lookup, body from notes;
drop table notes;
alter table new_notes rename to notes;
-- notes.lookup is renamed rowid, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table notes rename column lookup to rowid;
insert into notes_fts (notes_fts) values ('rebuild');
