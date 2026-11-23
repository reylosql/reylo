-- loses data: measurements.p2026
-- loses data: measurements.p2028
-- loses data: stores.psouth
alter table measurements drop partition p2026, p2028;
alter table stores drop partition psouth;
alter table stores add partition (partition peast values in ('e') engine = innodb);
