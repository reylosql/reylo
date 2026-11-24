alter table regional_sales add partition (partition `p east` values in (7, 8, 9) engine = innodb);
alter table t1 row_format=COMPRESSED;
