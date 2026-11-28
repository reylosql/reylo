alter table c drop foreign key `c.seq`, modify column seq int not null;
alter table c modify column seq int not null auto_increment;
alter table c add constraint `c.seq` foreign key (seq) references p (id);
