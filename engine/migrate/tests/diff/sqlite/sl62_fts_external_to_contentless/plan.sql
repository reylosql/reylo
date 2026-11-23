-- loses data: notes_fts
drop table notes_fts;
create virtual table notes_fts using fts5(body, content='');
