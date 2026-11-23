insert into notes (rowid, lookup, body) values (1, 42, 'running quickly');
insert into notes_fts (notes_fts) values ('rebuild');
