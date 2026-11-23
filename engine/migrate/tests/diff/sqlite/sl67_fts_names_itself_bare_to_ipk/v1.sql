create table notes (body text check ("notes"."body" <> ''));
create virtual table notes_fts using fts5(body, content='notes');
