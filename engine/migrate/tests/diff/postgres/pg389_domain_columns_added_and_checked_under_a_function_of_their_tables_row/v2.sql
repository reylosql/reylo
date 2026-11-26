create domain email as text check (value like '%@%');
create domain flag as boolean default false;
create domain code as text check (value <> '');
create table users (id integer primary key, code code, contact email, f flag);
create function user_key(u users) returns text language sql stable return 'user-' || u.id;
