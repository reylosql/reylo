create table users (id integer primary key, name text, active boolean not null default true, seq bigint generated always as identity, note text);
create function label(u users) returns text language sql stable return u.name || '#' || u.id;
create view labels as select label(u) as label from users u;
