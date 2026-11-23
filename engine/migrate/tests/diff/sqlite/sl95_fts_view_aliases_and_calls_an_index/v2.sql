create table notes (id integer primary key, draft text, body text generated always as ('published ' || draft) virtual);
create view notes_content as select filtered.id, filtered.body from notes as filtered;
create virtual table all_notes_fts using fts5(body, content='notes_content', content_rowid='id');
create view searchable as select notes.id as rowid, notes.body from notes join 'all_notes_fts'('published') as hit on hit.rowid = notes.id;
create virtual table filtered using fts4(body, content='searchable');
