alter table stores partition by list columns(region) (partition pnorth values in ('n') engine = innodb, partition psouth values in ('s') engine = innodb);
