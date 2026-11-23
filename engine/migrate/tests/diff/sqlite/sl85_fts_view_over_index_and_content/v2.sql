create table notes (id integer primary key, body text not null);
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id', tokenize='porter');
create view hits as select n.id, n.body, f.rank from notes n join notes_fts f on f.rowid = n.id;
