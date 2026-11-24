insert into posts (rowid, title, body) values (10, 'a', 'alpha beta'), (20, 'b', 'gamma delta');
insert into posts_fts (posts_fts) values ('rebuild');
insert into notes (rowid, body) values (7, 'kept rowid');
insert into notes_fts (notes_fts) values ('rebuild');
