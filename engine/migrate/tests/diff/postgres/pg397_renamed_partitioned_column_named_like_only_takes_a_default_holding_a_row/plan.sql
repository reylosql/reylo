alter table a rename column x to "y alter table only q";
alter table a alter column "y alter table only q" type text collate pg_catalog."POSIX";
alter table z alter column n type bigint;
alter table a alter column "y alter table only q" set default (row(0, 0)::z).id::text;
alter table a1 alter column "y alter table only q" set default ((row(0, (0)::bigint)::z).id)::text;
