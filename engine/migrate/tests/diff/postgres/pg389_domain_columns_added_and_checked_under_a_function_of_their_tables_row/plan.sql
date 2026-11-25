set check_function_bodies = off;
drop function user_key(users);
alter domain code add constraint code_check check (value <> '');
alter table users add column contact email;
alter table users add column f flag;
create function user_key(u users) returns text language sql stable return 'user-' || u.id;
reset check_function_bodies;
