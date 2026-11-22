-- file 1 of 3
alter type region add value 'ap' after 'us';
-- file 2 of 3
create index concurrently t_ap on t (id) where r = 'ap';
-- file 3 of 3
create policy only_ap on t using (r = 'ap');
