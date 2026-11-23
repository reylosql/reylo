insert into notes values (10, 'alpha one'), (30, 'gamma three');
insert into notes_fts (rowid, body) select id, body from notes;
