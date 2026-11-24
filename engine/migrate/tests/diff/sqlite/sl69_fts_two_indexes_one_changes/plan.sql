drop table title_fts;
create table new_notes (id integer primary key, title text, body text);
insert into new_notes (title, body) select title, body from notes;
drop table notes;
alter table new_notes rename to notes;
create virtual table title_fts using fts4(title, content="notes", tokenize=porter);
insert into body_fts (body_fts) values ('rebuild');
insert into title_fts (title_fts) values ('rebuild');
