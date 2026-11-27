-- A subquery that reads nothing of the query around it is one value: SQLite runs it once and takes its first row, or NULL. A unique key equated to one is pinned, and so is one IN a subquery of one row at most. A correlated subquery is a value per row, and a subquery of several rows matches several keys.
create table users (id integer primary key, email text unique, name text not null);
create table posts (id integer primary key, user_id integer not null, title text not null collate nocase);
create table tags (tag text primary key collate nocase);
insert into users values (1, 'a@x', 'a'), (2, null, 'b'), (3, 'c@x', 'c');
insert into posts values (1, 1, 'x'), (2, 1, 'X'), (3, 2, 'z'), (4, 3, 'c'), (5, 2, 'b');
insert into tags values ('x'), ('y');
