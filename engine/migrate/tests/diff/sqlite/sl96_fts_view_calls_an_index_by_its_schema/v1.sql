create table notes (id integer primary key, draft text, body text generated always as ('draft ' || draft) virtual);
create view notes_content as select n.id, n.body from notes as n;
create virtual table z_notes_fts using fts5(body, content='notes_content', content_rowid='id');
create view searchable as select notes.id as rowid, notes.body from notes join main.z_notes_fts('draft') as hit on hit.rowid = notes.id;
create virtual table filtered using fts4(body, content='searchable');
