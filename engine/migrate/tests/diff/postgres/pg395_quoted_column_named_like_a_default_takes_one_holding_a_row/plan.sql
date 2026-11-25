alter table a alter column "n set default x" drop default;
alter table a alter column "n set default x" type bigint;
alter table z add column y integer default (row(0, 0)::a).id;
alter table a alter column "n set default x" set default (row(0, 0, 0)::z).id;
