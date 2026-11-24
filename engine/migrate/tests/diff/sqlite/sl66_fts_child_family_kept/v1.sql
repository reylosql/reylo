create table users (id integer primary key, name text);
create table notes (body text, user_id integer references users (id) on delete cascade);
create virtual table notes_fts using fts5(body, content='notes');
create virtual table notes_fts4 using fts4(body, content=notes);
