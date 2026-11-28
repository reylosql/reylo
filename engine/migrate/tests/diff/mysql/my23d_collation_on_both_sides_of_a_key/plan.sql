alter table c drop foreign key c_ibfk_1, rename index code to code;
alter table r drop foreign key r_code;
alter table c modify column code varchar(10) collate utf8mb4_bin;
alter table p modify column code varchar(10) collate utf8mb4_bin not null;
alter table q modify column code varchar(10) not null;
alter table r modify column code varchar(10);
alter table c add constraint c_ibfk_1 foreign key (code) references p (code);
alter table r add constraint r_code foreign key (code) references q (code);
