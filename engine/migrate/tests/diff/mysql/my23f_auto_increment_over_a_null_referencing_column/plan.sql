alter table c modify column seq int not null;
alter table c drop foreign key c_ibfk_1, rename index seq to seq;
alter table c modify column seq int not null auto_increment;
alter table c add constraint c_ibfk_1 foreign key (seq) references p (id);
