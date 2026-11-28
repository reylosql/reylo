-- A write meets the constraints of each table its rows land in, and a column refuses NULL where
-- any of them refuses it there. A row computes a stored generated column as the partition it is
-- routed to does, which may differ from its parent (PostgreSQL 16 on). Every partition of
-- `events` computes `coalesce(p, 0) * 2`, never NULL, so `p` takes NULL, whatever the parent's
-- `p * 2` says; one of `mixed`'s computes the parent's `p * 2`, so there `p` refuses it.
create table events (region text not null, p int, gp int generated always as (p * 2) stored not null, q int, gq int generated always as (q + 1) stored not null) partition by list (region);
create table events_eu (region text not null, p int, gp int generated always as (coalesce(p, 0) * 2) stored not null, q int, gq int generated always as (q + 1) stored not null);
alter table events attach partition events_eu for values in ('eu');
create table events_rest (region text not null, p int, gp int generated always as (coalesce(p, 0) * 2) stored not null, q int, gq int generated always as (q + 1) stored not null);
alter table events attach partition events_rest default;
create table mixed (region text not null, p int, gp int generated always as (p * 2) stored not null) partition by list (region);
create table mixed_eu (region text not null, p int, gp int generated always as (coalesce(p, 0) * 2) stored not null);
alter table mixed attach partition mixed_eu for values in ('eu');
create table mixed_rest partition of mixed default;
-- An INSERT into a table with children lands in the table itself, an UPDATE without ONLY in its
-- children's rows too, and an UPDATE ONLY in its own, each write apart, as an UPDATE in a WITH
-- query beside the INSERT is. The parent's NO INHERIT CHECK refuses NULL
-- in `x` in its own rows, and the child's CHECK in `y` in the child's.
create table parent (id int primary key, x int check (x is not null) no inherit, y int);
create table child (check (coalesce(y, 0) > 0)) inherits (parent);
