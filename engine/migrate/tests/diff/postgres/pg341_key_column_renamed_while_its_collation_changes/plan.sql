-- file 1 of 4
alter table children drop constraint children_parent_id_fkey;
alter table parents drop constraint parents_pkey;
alter table children alter column parent_id type text collate ci;
-- parents.old_id is renamed id, keeping its rows. If they are two columns, write instead:
--   alter table parents drop column old_id;
--   alter table parents add column id text collate ci not null;
alter table parents rename column old_id to id;
alter table parents rename constraint parents_old_id_not_null to parents_id_not_null;
alter table parents alter column id type text collate ci;
-- file 2 of 4
create unique index concurrently parents_pkey on parents (id);
-- file 3 of 4
alter table parents add constraint parents_pkey primary key using index parents_pkey;
alter table children add constraint children_parent_id_fkey foreign key (parent_id) references parents(id) not valid;
-- file 4 of 4
alter table children validate constraint children_parent_id_fkey;
