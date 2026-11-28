-- A scalar subquery is one value, which the server refuses to hold more than one row, and so is a subquery of one row at most that IN compares with: a unique key equated to either is pinned where both sides are exact numbers. A correlated subquery is a value per row, and IN over several rows matches several keys. A SELECT with no FROM, or FROM DUAL, has one row for its WHERE to keep or remove.
create table users (id int primary key, email varchar(100) unique, name varchar(100) not null);
create table posts (id int primary key, user_id int not null, title varchar(100) not null);
insert into users values (1, 'a@x', 'a'), (2, null, 'b'), (3, 'c@x', 'c');
insert into posts values (1, 1, 'x'), (2, 1, 'y'), (3, 2, 'z');
create table input (name varchar(100) primary key);
insert into input values ('a');
create table singleton (n int primary key);
insert into singleton values (1);
create function pick() returns int not deterministic no sql return floor(rand() * 3) + 1;
