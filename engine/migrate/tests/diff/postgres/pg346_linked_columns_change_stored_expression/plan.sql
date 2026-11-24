-- file 1 of 2
alter table children drop constraint children_parent_code_fkey;
alter table children alter column parent_code set expression as (raw + 2);
alter table parents alter column code set expression as (raw + 2);
alter table children add constraint children_parent_code_fkey foreign key (parent_code) references parents(code) not valid;
-- file 2 of 2
alter table children validate constraint children_parent_code_fkey;
