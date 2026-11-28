-- name is not null with no default, so the copy fails on a table that has rows: give it a default, or add it nullable, fill it, then make it not null
-- b_email is not null with no default, so the copy fails on a table that has rows: give it a default, or add it nullable, fill it, then make it not null
-- email is not null with no default, so the copy fails on a table that has rows: give it a default, or add it nullable, fill it, then make it not null
-- a_name is not null with no default, so the copy fails on a table that has rows: give it a default, or add it nullable, fill it, then make it not null
-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer not null, name text not null, b_email text not null, constraint a_b_email_fkey foreign key (b_email) references new_b (email));
create table new_b (id integer not null, email text not null, a_name text not null, constraint b_a_name_fkey foreign key (a_name) references new_a (name));
create unique index new_a_name_key on new_a (name);
create unique index new_b_email_key on new_b (email);
insert into new_a (id) select id from a;
insert into new_b (id) select id from b;
drop table b;
drop table a;
alter table new_a rename to a;
alter table new_b rename to b;
drop index new_a_name_key;
drop index new_b_email_key;
create unique index a_name_key on a (name);
create unique index b_email_key on b (email);
