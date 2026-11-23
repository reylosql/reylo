-- file 1 of 2
alter type visibility add value 'org' after 'team';
-- file 2 of 2
alter table bookmarks alter column visibility set default 'org';
