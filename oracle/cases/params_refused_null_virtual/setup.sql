-- A partition may compute a virtual generated column its own way too (PostgreSQL 18). The parent's
-- `p * 2` is NULL wherever `p` is, but its one partition's `coalesce(p, 0) * 2` is not, so `p`
-- takes NULL; both compute `q + 1`, so `q` refuses it.
create table readings (region text not null, p int, gp int generated always as (p * 2) virtual not null, q int, gq int generated always as (q + 1) virtual not null) partition by list (region);
create table readings_all partition of readings (gp generated always as (coalesce(p, 0) * 2) virtual) default;
