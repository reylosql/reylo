-- file 1 of 4
alter table children drop constraint children_parent_code_fkey;
alter table parents drop constraint parents_code_key;
alter table children alter column parent_code set expression as (raw + 2);
-- parents.old_code is renamed code, keeping its rows. If they are two columns, write instead:
--   alter table parents drop column old_code;
--   alter table parents add column code integer generated always as (raw + 2) stored;
alter table parents rename column old_code to code;
alter table parents alter column code set expression as (raw + 2);
-- file 2 of 4
create unique index concurrently parents_code_key on parents (code);
-- file 3 of 4
alter table parents add constraint parents_code_key unique using index parents_code_key;
alter table children add constraint children_parent_code_fkey foreign key (parent_code) references parents(code) not valid;
-- file 4 of 4
alter table children validate constraint children_parent_code_fkey;
