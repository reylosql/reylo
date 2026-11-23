alter default privileges grant select on tables to diff_reader;
create table t (id integer primary key);
alter table t owner to diff_owner;
revoke select on t from diff_reader;
alter default privileges for role diff_owner grant select on tables to diff_reader;
