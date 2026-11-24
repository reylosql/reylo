-- file 1 of 2
alter table children drop constraint children_parent_id_fkey;
alter table children alter column parent_id type text collate ci;
alter table parents alter column id type text collate ci;
alter table children add constraint children_parent_id_fkey foreign key (parent_id) references parents(id) not valid;
-- file 2 of 2
alter table children validate constraint children_parent_id_fkey;
