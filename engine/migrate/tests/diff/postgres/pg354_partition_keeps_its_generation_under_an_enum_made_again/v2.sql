create type mood as enum ('a', 'b');
create table t (id int, m mood generated always as ('a'::mood) stored) partition by list (id);
create table t1 (id int, m mood generated always as ('b'::mood) stored);
alter table t attach partition t1 for values in (1);
