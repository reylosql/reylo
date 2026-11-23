create type mood as enum ('a');
create table items (i integer primary key, m mood default 'a');
create table old_notes (body text);
create view live_view as select 1 as x;
