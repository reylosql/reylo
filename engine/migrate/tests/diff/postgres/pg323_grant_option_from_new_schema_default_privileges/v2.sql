alter default privileges grant select on tables to diff_reader;
alter default privileges in schema public grant select on tables to diff_reader with grant option;
create table t (id integer primary key);
