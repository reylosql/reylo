create table t (id int primary key, seq int generated always as (id + 1) stored, key (seq));
