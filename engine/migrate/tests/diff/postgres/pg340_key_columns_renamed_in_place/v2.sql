create table parents (parent_id uuid primary key default gen_random_uuid(), slug text not null, constraint parents_code_key unique nulls not distinct (slug) include (parent_id));
create index parents_code_idx on parents (slug) where slug <> '';
create table children (id uuid primary key default gen_random_uuid(), parent_id uuid not null references parents(parent_id) on delete cascade deferrable, up uuid references children(id));
create index children_parent_idx on children using btree (parent_id desc nulls last) include (id);
create table pt (b int, v int, constraint pt_key unique (b, v)) partition by range (v);
create table pt1 partition of pt for values from (0) to (10);
create index pti on pt (b);
