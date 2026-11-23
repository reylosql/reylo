alter default privileges grant select on tables to diff_reader with grant option;
create table t (id integer primary key);
revoke grant option for select on t from diff_reader;
