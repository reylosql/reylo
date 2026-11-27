-- A scalar subquery is one value, which the server refuses to hold more than one row, and so is a subquery of one row at most that IN compares with. A unique key equated to either is pinned, wherever the subquery reads a WITH query by bare column names. A subquery of several rows under IN matches several keys, and a correlated one is a value per row.
create table users (id int primary key, email text unique, name text not null);
create table posts (id int primary key, user_id int not null, title text not null);
insert into users values (1, 'a@x', 'a'), (2, null, 'b'), (3, 'c@x', 'c');
insert into posts values (1, 1, 'x'), (2, 1, 'y'), (3, 2, 'z');
create view recent_posts as select id, user_id from posts where id > 0;
create sequence numbering;
create view numbered_posts as select p.id as post_id, nextval('numbering')::int as id from posts p;
