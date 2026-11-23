insert into notes (rowid, title, body) values (10, 'first', 'alpha one'), (30, 'second', 'gamma three');
insert into body_fts (body_fts) values ('rebuild');
insert into title_fts (title_fts) values ('rebuild');
