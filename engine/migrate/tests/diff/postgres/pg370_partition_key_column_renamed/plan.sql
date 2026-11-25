alter table h drop constraint h_pkey;
-- h.region is renamed area, keeping its rows. If they are two columns, write instead:
--   alter table h drop column region;
--   alter table h add column area text not null;
alter table h rename column region to area;
alter table h rename constraint h_region_not_null to h_area_not_null;
-- m.ts is renamed created_on, keeping its rows. If they are two columns, write instead:
--   alter table m drop column ts;
--   alter table m add column created_on date;
alter table m rename column ts to created_on;
alter table h add constraint h_pkey primary key (id, area);
