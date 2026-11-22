-- file 1 of 2
alter type priority add value 'urgent' after 'high';
-- file 2 of 2
drop view high_tasks;
create view high_tasks as select id from tasks where priority in ('high', 'urgent');
