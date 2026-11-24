create table notes (body text not null check ("notes"."body" <> ''));
create virtual table notes_fts using fts4(body, content=notes);
