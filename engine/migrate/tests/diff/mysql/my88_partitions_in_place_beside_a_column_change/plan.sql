-- loses data: m.p1
alter table m drop partition p1;
alter table m add column note varchar(20);
alter table m add partition (partition p4 values less than (2029) engine = innodb);
