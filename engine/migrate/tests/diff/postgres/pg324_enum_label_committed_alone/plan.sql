-- file 1 of 2
alter type mood add value 'b' after 'a';
-- file 2 of 2
-- loses data: old_notes
drop view live_view;
drop table old_notes;
alter table items alter column m set default 'b';
create view live_view as select 2 as x;
