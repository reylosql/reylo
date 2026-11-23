create table posts (title text, body text);
create virtual table posts_fts using fts4(body, content='posts');
create table notes (body text);
create virtual table notes_fts using fts5(body, content=notes);
