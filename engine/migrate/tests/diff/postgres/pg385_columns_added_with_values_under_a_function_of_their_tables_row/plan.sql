set check_function_bodies = off;
drop view labels;
drop function label(users);
alter table users add column active boolean default true not null;
alter table users add column note text;
alter table users add column seq bigint generated always as identity not null;
create function label(u users) returns text language sql stable return u.name || '#' || u.id;
create view labels as select label(u) as label from users u;
reset check_function_bodies;
