create table "x0 x" (
  id integer not null,
  c2 text,
  constraint "x0 x_pkey" primary key (id)
);
create index x1 on "x0 x" (lower(c2));
alter index x1 alter column 1 set statistics 1000;
