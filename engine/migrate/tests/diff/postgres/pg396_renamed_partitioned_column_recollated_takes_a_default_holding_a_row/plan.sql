alter table a rename column x to y;
alter table a alter column y type text collate pg_catalog."POSIX";
alter table z alter column n type bigint;
alter table a alter column y set default (row(0, 0)::z).id::text;
alter table a1 alter column y set default ((row(0, (0)::bigint)::z).id)::text;
