create table posts (id integer primary key, title text, body text);
create virtual table posts_fts using fts4(body, content='posts');
create table notes (body text, check (body <> ''));
create virtual table notes_fts using fts5(body, content=notes);
