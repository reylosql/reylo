-- file 1 of 2
alter type mood add value 'b' after 'a';
-- file 2 of 2
-- loses data: z.backup
alter table z drop column backup;
drop view v;
alter table x alter column c type text;
alter table y alter column c set default 'b';
create view v as select c from x;
