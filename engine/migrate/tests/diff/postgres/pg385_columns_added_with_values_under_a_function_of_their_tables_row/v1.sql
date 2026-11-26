create table users (id integer primary key, name text);
create function label(u users) returns text language sql stable return u.name || '#' || u.id;
create view labels as select label(u) as label from users u;
