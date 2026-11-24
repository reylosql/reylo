create table parents (raw int not null, code int generated always as (raw + 2) stored, constraint parents_code_key unique (code));
create table children (raw int not null, parent_code int generated always as (raw + 2) stored, constraint children_parent_code_fkey foreign key (parent_code) references parents(code));
