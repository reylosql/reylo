-- q."old""id" is renamed new_id, keeping its rows. If they are two columns, write instead:
--   alter table q drop column "old""id";
--   alter table q add column new_id integer;
alter table q rename column "old""id" to new_id;
