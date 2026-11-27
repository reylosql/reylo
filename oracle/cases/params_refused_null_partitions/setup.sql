-- A row computes a stored generated column as the table it lands in does, and a partition may
-- compute it its own way (PostgreSQL 16 on). Here the parent's `p * 2` is NULL wherever `p` is,
-- but every partition's `coalesce(p, 0) * 2` is not, so `p` takes NULL; `q`'s partitions compute
-- `q + 1` as the parent does, so `q` refuses it.
create table events (region text not null, p int, gp int generated always as (p * 2) stored not null, q int, gq int generated always as (q + 1) stored not null) partition by list (region);
create table events_eu (region text not null, p int, gp int generated always as (coalesce(p, 0) * 2) stored not null, q int, gq int generated always as (q + 1) stored not null);
alter table events attach partition events_eu for values in ('eu');
create table events_rest (region text not null, p int, gp int generated always as (coalesce(p, 0) * 2) stored not null, q int, gq int generated always as (q + 1) stored not null);
alter table events attach partition events_rest default;
