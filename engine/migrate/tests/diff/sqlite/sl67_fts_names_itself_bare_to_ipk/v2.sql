create table notes (id integer primary key, body text check ("notes"."body" <> ''));
create virtual table notes_fts using fts5(body, content='notes');
