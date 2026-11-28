alter table c drop foreign key c_seq, modify column seq int not null;
alter table c modify column seq int not null auto_increment;
alter table c add constraint c_seq foreign key (seq) references p (id);
