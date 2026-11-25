create table m (id int, created_on date) partition by range (created_on);
create table m_2025 partition of m for values from ('2025-01-01') to ('2026-01-01');
create table m_def partition of m default;
create table h (id int, area text, primary key (id, area)) partition by list (area);
create table h_eu partition of h for values in ('eu') partition by hash (id);
create table h_eu_0 partition of h_eu for values with (modulus 1, remainder 0);
