create table parents (raw int not null, code int, constraint parents_code_key unique (code));
create table children (id int primary key, parent_code int, constraint children_parent_code_fkey foreign key (parent_code) references parents(code));
