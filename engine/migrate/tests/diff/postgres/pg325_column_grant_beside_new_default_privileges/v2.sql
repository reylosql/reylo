create table kept (id integer primary key);
create table t (id integer primary key, secret text);
grant select (id) on t to diff_reader;
alter default privileges grant select on tables to diff_reader;
