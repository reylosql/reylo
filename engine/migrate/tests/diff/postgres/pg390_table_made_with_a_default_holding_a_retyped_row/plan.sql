alter table t alter column n type bigint;
create table u (
  id integer not null,
  x text default row(0, 0)::t::text,
  constraint u_pkey primary key (id)
);
