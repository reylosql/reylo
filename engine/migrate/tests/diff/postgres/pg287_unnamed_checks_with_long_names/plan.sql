-- file 1 of 3
create table "表格" (
  id integer,
  constraint "表格_id_check" check (id > 0)
);
create table tttttttttttttttttttttttttttttt (
  aaaaaaaaaaaaaaaaaaaaaaaaaaaa_x integer,
  aaaaaaaaaaaaaaaaaaaaaaaaaaaa_y integer,
  constraint tttttttttttttttttttttttttttt_aaaaaaaaaaaaaaaaaaaaaaaaaaa_check1 check (aaaaaaaaaaaaaaaaaaaaaaaaaaaa_y > 0),
  constraint tttttttttttttttttttttttttttt_aaaaaaaaaaaaaaaaaaaaaaaaaaaa_check check (aaaaaaaaaaaaaaaaaaaaaaaaaaaa_x > 0)
);
-- file 2 of 3
create unique index concurrently other_id_key on other (id);
-- file 3 of 3
alter table other add constraint other_id_key unique using index other_id_key deferrable;
