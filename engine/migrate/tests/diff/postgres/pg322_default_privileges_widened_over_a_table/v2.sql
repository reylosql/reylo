create table t (id integer primary key);
alter default privileges grant select, insert on tables to diff_reporting;
alter default privileges grant usage on sequences to diff_reporting;
