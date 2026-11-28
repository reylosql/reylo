create table p (id int primary key); create table c (id int primary key, seq int not null auto_increment, constraint c_seq foreign key (seq) references p (id));
