create table p (id int primary key); create table c (id int primary key, seq int not null auto_increment, foreign key (seq) references p (id));
