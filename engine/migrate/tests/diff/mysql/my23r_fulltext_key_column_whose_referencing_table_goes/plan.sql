-- loses data: z
drop table z;
alter table a drop foreign key a_p, rename index seq to seq, modify column seq int not null;
alter table a modify column seq int not null auto_increment;
alter table a add constraint a_p foreign key (seq) references p (id);
