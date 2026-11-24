insert into notes values (1, 100, 'first', 'running quickly'), (2, 200, 'second', 'walking slowly');
insert into notes_fts (notes_fts) values ('rebuild');
