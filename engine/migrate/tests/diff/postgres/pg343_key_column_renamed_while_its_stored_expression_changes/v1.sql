create table parents (raw int not null, old_code int generated always as (raw + 1) stored, constraint parents_code_key unique (old_code));
create table children (raw int not null, parent_code int generated always as (raw + 1) stored, constraint children_parent_code_fkey foreign key (parent_code) references parents(old_code));
