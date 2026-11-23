create table new_users (id integer primary key, name text not null);
create table new_notes (body text, user_id integer references new_users (id) on delete cascade);
insert into new_users (id, name) select id, name from users;
insert into new_notes (rowid, body, user_id) select rowid, body, user_id from notes;
drop table notes;
drop table users;
alter table new_users rename to users;
alter table new_notes rename to notes;
