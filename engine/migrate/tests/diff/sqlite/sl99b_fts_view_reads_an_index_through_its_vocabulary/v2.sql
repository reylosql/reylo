create table notes (id integer primary key, body text);
create view z_source as select id, body from notes;
create virtual table z_root using fts5(body, content='z_source', content_rowid='id');
create virtual table vocab using fts5vocab(z_root, 'instance');
create view hits as select notes.id as rowid, notes.body from notes join vocab on vocab.doc = notes.id where vocab.term = 'published';
create virtual table a_leaf using fts4(body, content='hits');
