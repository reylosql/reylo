alter table c drop foreign key c_ibfk_1, rename index parent to parent;
alter table d drop foreign key d_parent;
alter table p drop foreign key p_ibfk_1, rename index up to up;
alter table p modify column id int not null auto_increment;
alter table c add constraint c_ibfk_1 foreign key (parent) references p (id);
alter table d add constraint d_parent foreign key (parent) references p (id);
alter table p add constraint p_ibfk_1 foreign key (up) references p (id);
