-- m.ts is renamed created_on, keeping its rows. If they are two columns, write instead:
--   alter table m drop column ts;
--   alter table m add column created_on date;
alter table m rename column ts to created_on;
alter table m alter column old_payload type bigint;
