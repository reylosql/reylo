insert into users values (1, 'ann');
insert into notes (rowid, body, user_id) values (10, 'alpha one', 1), (30, 'gamma three', 1);
insert into notes_fts (notes_fts) values ('rebuild');
