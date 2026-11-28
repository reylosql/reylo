-- loses data: spare
alter table c drop foreign key c_seq, modify column seq int not null;
drop table spare;
alter table c modify column seq int not null auto_increment;
alter table c add constraint c_seq foreign key (seq) references p (id);
