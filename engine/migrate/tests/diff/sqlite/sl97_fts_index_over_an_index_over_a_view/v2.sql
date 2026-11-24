create table notes (id integer primary key, body text);
create view z_source as select id, body from notes;
create virtual table z_root using fts5(body, content='z_source', content_rowid='id');
create view hits as select notes.id as rowid, notes.body from notes join z_root on z_root.rowid = notes.id where z_root match 'published';
create virtual table m_index using fts4(body, content='hits');
create virtual table a_leaf using fts4(body, content='m_index');
