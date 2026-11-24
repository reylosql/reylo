insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
insert into notes values (9, 'added later');
insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
delete from notes where id = 9;
insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
