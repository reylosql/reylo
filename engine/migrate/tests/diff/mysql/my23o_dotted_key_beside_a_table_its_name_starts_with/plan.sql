alter table c drop foreign key `seq.key`, modify column seq int not null;
alter table c modify column seq int not null auto_increment;
alter table c add constraint `seq.key` foreign key (seq) references p (id);
