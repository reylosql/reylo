-- loses data: p
-- loses data: z
alter table a drop foreign key a_p, rename index seq to seq;
drop table p;
drop table z;
alter table a modify column seq int not null;
alter table a modify column seq int not null auto_increment;
