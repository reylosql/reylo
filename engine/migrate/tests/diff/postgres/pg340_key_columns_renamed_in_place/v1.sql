create table parents (id uuid primary key default gen_random_uuid(), code text not null, constraint parents_code_key unique nulls not distinct (code) include (id));
create index parents_code_idx on parents (code) where code <> '';
create table children (child_id uuid primary key default gen_random_uuid(), parent_id uuid not null references parents(id) on delete cascade deferrable, up uuid references children(child_id));
create index children_parent_idx on children using btree (parent_id desc nulls last) include (child_id);
create table pt (a int, v int, constraint pt_key unique (a, v)) partition by range (v);
create table pt1 partition of pt for values from (0) to (10);
create index pti on pt (a);
