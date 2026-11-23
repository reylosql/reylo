create table t (
  id integer not null,
  secret text,
  constraint t_pkey primary key (id)
);
alter default privileges grant select on tables to diff_reader;
revoke select on table t from diff_reader;
grant select (id) on table t to diff_reader;
