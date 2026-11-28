alter table p modify column u int not null;
alter table c drop foreign key c_ibfk_1, rename index pu to pu;
alter table p modify column u int not null auto_increment;
alter table c add constraint c_ibfk_1 foreign key (pu) references p (u);
