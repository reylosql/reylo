create table users (id integer primary key, name text);
create table notes (body text, user_id integer references users (id));
create virtual table notes_fts using fts5(body, content='notes');
