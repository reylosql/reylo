-- file 1 of 5
drop index pti;
alter table pt drop constraint pt_key;
-- children.child_id is renamed id, keeping its rows. If they are two columns, write instead:
--   alter table children drop column child_id;
--   alter table children add column id uuid default gen_random_uuid() not null;
alter table children rename column child_id to id;
alter table children rename constraint children_child_id_not_null to children_id_not_null;
-- parents.id is renamed parent_id, keeping its rows. If they are two columns, write instead:
--   alter table parents drop column id;
--   alter table parents add column parent_id uuid default gen_random_uuid() not null;
alter table parents rename column id to parent_id;
alter table parents rename constraint parents_id_not_null to parents_parent_id_not_null;
-- parents.code is renamed slug, keeping its rows. If they are two columns, write instead:
--   alter table parents drop column code;
--   alter table parents add column slug text not null;
alter table parents rename column code to slug;
alter table parents rename constraint parents_code_not_null to parents_slug_not_null;
-- pt.a is renamed b, keeping its rows. If they are two columns, write instead:
--   alter table pt drop column a;
--   alter table pt add column b integer;
alter table pt rename column a to b;
alter table pt add constraint pt_key unique (b, v);
-- file 2 of 5
create index concurrently parents_code_idx_new on parents (slug) where slug <> '';
-- file 3 of 5
drop index parents_code_idx;
alter index parents_code_idx_new rename to parents_code_idx;
-- file 4 of 5
create index concurrently pt1_b_idx on pt1 using btree (b);
-- file 5 of 5
create index pti on pt using btree (b);
