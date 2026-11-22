create table t (
  id integer not null,
  constraint t_pkey primary key (id)
);
alter default privileges in schema public grant select on tables to diff_reader with grant option;
grant select on table t to diff_reader with grant option;
