-- file 1 of 2
-- accounts.old_code is renamed code, keeping its rows. If they are two columns, write instead:
--   alter table accounts drop column old_code;
--   alter table accounts add column code integer not null;
alter table accounts rename column old_code to code;
alter table accounts add constraint code_not_null check (code is not null) not valid;
-- file 2 of 2
alter table accounts validate constraint code_not_null;
alter table accounts alter column code set not null;
alter table accounts drop constraint code_not_null;
