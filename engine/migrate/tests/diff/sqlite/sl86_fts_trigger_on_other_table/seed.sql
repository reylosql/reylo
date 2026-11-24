insert into notes (rowid, body) values (10, 'alpha one'), (30, 'gamma three');
insert into notes_fts (notes_fts) values ('rebuild');
