-- file 1 of 2
alter table children drop constraint children_parent_id_fkey;
alter table parents drop constraint parents_pkey;
alter table children alter column parent_id type text collate ci;
-- parents.old_id is renamed id, keeping its rows. If they are two columns, write instead:
--   alter table parents drop column old_id;
--   alter table parents add column id text collate ci not null;
alter table parents rename column old_id to id;
alter table parents rename constraint parents_old_id_not_null to parents_id_not_null;
alter table parents alter column id type text collate ci;
alter table parents add constraint parents_pkey primary key (id);
alter table children add constraint children_parent_id_fkey foreign key (parent_id) references parents(id) not valid;
-- file 2 of 2
alter table children validate constraint children_parent_id_fkey;
