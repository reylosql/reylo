create type mood as enum ('a', 'b');
create table items (i integer primary key, m mood default 'b');
create view live_view as select 2 as x;
