alter table a rename column x to y;
alter table a alter column y drop default;
alter table a alter column y type bigint;
alter table z alter column n type bigint;
alter table a alter column y set default (row(0, 0)::z).id;
