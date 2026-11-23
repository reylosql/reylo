create table notes (id integer primary key, draft text, body text generated always as ('published ' || draft) virtual);
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');
create view searchable as select notes.id as rowid, notes.body from notes join notes_fts on notes_fts.rowid = notes.id where notes_fts match 'published';
create virtual table filtered_fts using fts4(body, content='searchable');
