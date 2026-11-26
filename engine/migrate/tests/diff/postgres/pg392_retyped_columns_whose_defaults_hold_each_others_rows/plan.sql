alter table a alter column n drop default;
alter table b alter column n drop default;
alter table a alter column n drop default;
alter table a alter column n type bigint;
alter table b alter column n drop default;
alter table b alter column n type bigint;
alter table a alter column n set default (row(0, 0)::b).id;
alter table b alter column n set default (row(0, (0)::bigint)::a).id;
