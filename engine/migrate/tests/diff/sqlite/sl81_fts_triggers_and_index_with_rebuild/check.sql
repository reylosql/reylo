insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
insert into notes (id, body) values (99, 'added later');
insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
delete from notes where id = 99;
insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
