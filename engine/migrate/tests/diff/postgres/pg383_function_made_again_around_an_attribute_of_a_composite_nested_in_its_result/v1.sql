create type p as (a integer);
create type q as (x p, tag text);
create domain d as p;
create function nested() returns q language sql immutable return row(row(1)::p, 'one')::q;
create function listed() returns p[] language sql immutable return array[row(1)::p, row(2)::p];
create function wrapped() returns d language sql immutable return row(3)::p::d;
create table kept (id integer primary key);
