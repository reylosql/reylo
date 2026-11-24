create type mood as enum ('a', 'b', 'c');
create table t (id int primary key, m mood generated always as ('a'::mood) stored, n mood);
