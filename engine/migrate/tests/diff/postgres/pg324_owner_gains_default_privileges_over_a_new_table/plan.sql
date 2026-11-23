create table t (
  id integer not null,
  constraint t_pkey primary key (id)
);
alter default privileges for role diff_owner grant select on tables to diff_reader;
alter table t owner to diff_owner;
revoke select on table t from diff_reader;
