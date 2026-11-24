-- accounts.old_code is renamed code, keeping its rows. If they are two columns, write instead:
--   alter table accounts drop column old_code;
--   alter table accounts add column code integer default 2;
alter table accounts rename column old_code to code;
alter table accounts alter column code set default 2;
