create table new_notes (body text, check (body <> ''));
insert into new_notes (rowid, body) select rowid, body from notes;
drop table notes;
alter table new_notes rename to notes;
create table new_posts (id integer primary key, title text, body text);
insert into new_posts (title, body) select title, body from posts;
drop table posts;
alter table new_posts rename to posts;
insert into posts_fts (posts_fts) values ('rebuild');
