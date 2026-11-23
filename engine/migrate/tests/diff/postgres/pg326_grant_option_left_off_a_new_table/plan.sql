create table t (
  id integer not null,
  constraint t_pkey primary key (id)
);
grant select on table t to diff_reader;
revoke grant option for select on table t from diff_reader;
