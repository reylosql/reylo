create table m (id int, created_on date, ts_day int, old_payload bigint) partition by range (created_on, ts_day);
create table m_1 partition of m for values from ('2025-01-01', 0) to ('2026-01-01', 0);
