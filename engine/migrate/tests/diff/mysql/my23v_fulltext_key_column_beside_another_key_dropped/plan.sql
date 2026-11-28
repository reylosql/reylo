alter table b drop foreign key b_p, rename index b_pid to b_pid;
alter table z drop foreign key z_a;
alter table a drop foreign key a_p, rename index seq to seq, modify column seq int not null;
alter table a modify column seq int not null auto_increment;
alter table a add constraint a_p foreign key (seq) references p (id);
alter table z add constraint z_a foreign key (a_seq) references a (seq);
